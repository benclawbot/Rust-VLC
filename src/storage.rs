use crate::model::{MediaDetails, Track};
use crate::playlists::Playlist;
use rusqlite::{params, Connection, OptionalExtension, MAIN_DB};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const SCHEMA_VERSION: i64 = 2;

#[derive(Clone, Debug)]
pub struct LibraryData {
    pub tracks: Vec<Track>,
    pub movies: Vec<Track>,
    pub saved_movie_ids: Vec<i32>,
    pub playlists: Vec<Playlist>,
    pub selected_playlist: Option<i32>,
    pub current_view: String,
    pub volume: i32,
    pub rate: f32,
    pub progress: Vec<Progress>,
    pub details: Vec<(i32, MediaDetails)>,
}

impl Default for LibraryData {
    fn default() -> Self {
        Self {
            tracks: Vec::new(),
            movies: Vec::new(),
            saved_movie_ids: Vec::new(),
            playlists: Vec::new(),
            selected_playlist: None,
            current_view: "movies".into(),
            volume: 100,
            rate: 1.0,
            progress: Vec::new(),
            details: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Progress {
    pub media_id: i32,
    pub position_ms: i64,
    pub duration_ms: i64,
    pub completed: bool,
}

pub fn application_directories() -> Result<(PathBuf, PathBuf), String> {
    #[cfg(debug_assertions)]
    if let Some(directory) = std::env::var_os("RUST_VLC_TEST_DATA_DIR") {
        let directory = PathBuf::from(directory);
        if !directory.is_absolute() {
            return Err("The test data directory must be absolute.".into());
        }
        let cache = directory.join("cache");
        std::fs::create_dir_all(&cache).map_err(|error| error.to_string())?;
        return Ok((directory.join("library.sqlite3"), cache));
    }
    let dirs = directories::ProjectDirs::from("org", "benclawbot", "Rust-VLC")
        .ok_or("The operating system did not provide an application data directory.")?;
    std::fs::create_dir_all(dirs.data_local_dir()).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(dirs.cache_dir()).map_err(|error| error.to_string())?;
    Ok((
        dirs.data_local_dir().join("library.sqlite3"),
        dirs.cache_dir().to_owned(),
    ))
}

pub struct LibraryStore {
    connection: Connection,
}

impl LibraryStore {
    pub fn open(path: &Path) -> Result<Self, String> {
        let existed = path.metadata().is_ok_and(|metadata| metadata.len() > 0);
        let mut connection = Connection::open(path).map_err(|error| error.to_string())?;
        connection
            .busy_timeout(Duration::from_millis(500))
            .map_err(|error| error.to_string())?;
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|error| error.to_string())?;
        if version > SCHEMA_VERSION {
            return Err(format!(
                "This library uses schema {version}; this build supports {SCHEMA_VERSION}. The database was preserved."
            ));
        }
        if version == 0 {
            let tables: i64 = connection.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'", [], |row| row.get(0),
            ).map_err(|error| error.to_string())?;
            if tables > 0 {
                return Err(
                    "The library has an unrecognized schema. The database was preserved.".into(),
                );
            }
            if existed {
                backup_before_migration(&connection, path)?;
            }
            let transaction = connection
                .transaction()
                .map_err(|error| error.to_string())?;
            transaction.execute_batch(
                "CREATE TABLE media (id INTEGER PRIMARY KEY, kind TEXT NOT NULL CHECK(kind IN ('audio','video')), path BLOB NOT NULL, encoding TEXT NOT NULL, title TEXT NOT NULL, format TEXT NOT NULL, UNIQUE(kind,encoding,path));
                 CREATE TABLE playlists (id INTEGER PRIMARY KEY, name TEXT NOT NULL COLLATE NOCASE UNIQUE);
                 CREATE TABLE playlist_items (playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE, position INTEGER NOT NULL, media_id INTEGER NOT NULL REFERENCES media(id), PRIMARY KEY(playlist_id,position));
                 CREATE TABLE progress (media_id INTEGER PRIMARY KEY REFERENCES media(id), position_ms INTEGER NOT NULL, duration_ms INTEGER NOT NULL, completed INTEGER NOT NULL);
                 CREATE TABLE details (media_id INTEGER PRIMARY KEY REFERENCES media(id), artist TEXT NOT NULL, album TEXT NOT NULL, album_artist TEXT NOT NULL, genre TEXT NOT NULL, artwork BLOB, art_encoding TEXT, art_override INTEGER NOT NULL, disc_number INTEGER, track_number INTEGER);
                 CREATE TABLE preferences (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 PRAGMA user_version=2;"
            ).map_err(|error| error.to_string())?;
            transaction.commit().map_err(|error| error.to_string())?;
        } else if version == 1 {
            backup_before_migration(&connection, path)?;
            let transaction = connection
                .transaction()
                .map_err(|error| error.to_string())?;
            transaction
                .execute_batch(
                    "ALTER TABLE details ADD COLUMN album_artist TEXT NOT NULL DEFAULT ''; PRAGMA user_version=2;",
                )
                .map_err(|error| error.to_string())?;
            transaction.commit().map_err(|error| error.to_string())?;
        }
        connection
            .pragma_update(None, "foreign_keys", true)
            .map_err(|error| error.to_string())?;
        Ok(Self { connection })
    }

    pub fn load(&self) -> Result<LibraryData, String> {
        let mut data = LibraryData::default();
        let mut statement = self
            .connection
            .prepare("SELECT id,kind,path,encoding,title,format FROM media ORDER BY id")
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i32>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })
            .map_err(|error| error.to_string())?;
        for row in rows {
            let (id, kind, bytes, encoding, title, format) =
                row.map_err(|error| error.to_string())?;
            let track = Track {
                id,
                path: decode_path(&bytes, &encoding)?,
                title,
                format,
            };
            if kind == "video" {
                data.movies.push(track);
            } else {
                data.tracks.push(track);
            }
        }
        let mut statement = self
            .connection
            .prepare("SELECT id,name FROM playlists ORDER BY id")
            .map_err(|error| error.to_string())?;
        for playlist in statement
            .query_map([], |row| {
                Ok(Playlist {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    track_ids: Vec::new(),
                })
            })
            .map_err(|error| error.to_string())?
        {
            let mut playlist = playlist.map_err(|error| error.to_string())?;
            let mut items = self
                .connection
                .prepare(
                    "SELECT media_id FROM playlist_items WHERE playlist_id=? ORDER BY position",
                )
                .map_err(|error| error.to_string())?;
            playlist.track_ids = items
                .query_map([playlist.id], |row| row.get(0))
                .map_err(|error| error.to_string())?
                .collect::<Result<_, _>>()
                .map_err(|error| error.to_string())?;
            data.playlists.push(playlist);
        }
        let mut statement = self
            .connection
            .prepare("SELECT media_id,position_ms,duration_ms,completed FROM progress")
            .map_err(|error| error.to_string())?;
        data.progress = statement
            .query_map([], |row| {
                Ok(Progress {
                    media_id: row.get(0)?,
                    position_ms: row.get(1)?,
                    duration_ms: row.get(2)?,
                    completed: row.get(3)?,
                })
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|error| error.to_string())?;
        let mut statement = self
            .connection
            .prepare(
                "SELECT media_id,artist,album,album_artist,genre,artwork,art_encoding,art_override,disc_number,track_number FROM details",
            )
            .map_err(|error| error.to_string())?;
        for row in statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i32>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<Vec<u8>>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, bool>(7)?,
                    row.get::<_, Option<u32>>(8)?,
                    row.get::<_, Option<u32>>(9)?,
                ))
            })
            .map_err(|error| error.to_string())?
        {
            let (
                id,
                artist,
                album,
                album_artist,
                genre,
                art,
                encoding,
                artwork_override,
                disc_number,
                track_number,
            ) = row.map_err(|error| error.to_string())?;
            let artwork = match (art, encoding) {
                (Some(bytes), Some(encoding)) => Some(decode_path(&bytes, &encoding)?),
                _ => None,
            };
            data.details.push((
                id,
                MediaDetails {
                    artist,
                    album,
                    album_artist,
                    genre,
                    disc_number,
                    track_number,
                    artwork,
                    artwork_override,
                },
            ));
        }
        let preference = |key: &str| {
            self.connection
                .query_row("SELECT value FROM preferences WHERE key=?", [key], |row| {
                    row.get::<_, String>(0)
                })
                .ok()
        };
        data.selected_playlist =
            preference("selected_playlist").and_then(|value| value.parse().ok());
        data.current_view = preference("view")
            .filter(|value| value == "movies" || value == "music")
            .unwrap_or(data.current_view);
        data.volume = preference("volume")
            .and_then(|value| value.parse().ok())
            .filter(|value| (0..=100).contains(value))
            .unwrap_or(100);
        data.rate = preference("rate")
            .and_then(|value| value.parse::<f32>().ok())
            .filter(|value| value.is_finite() && (0.25..=4.0).contains(value))
            .unwrap_or(1.0);
        let saved_movie_ids = self
            .connection
            .query_row(
                "SELECT value FROM preferences WHERE key=?",
                ["saved_movie_ids"],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| {
                format!(
                    "Saved movie IDs in library preferences could not be read ({error}); the database was preserved."
                )
            })?;
        data.saved_movie_ids = saved_movie_ids
            .map(|value| {
                serde_json::from_str::<Vec<i32>>(&value).map_err(|error| {
                    format!(
                        "Saved movie IDs in library preferences are invalid ({error}); the database was preserved."
                    )
                })
            })
            .transpose()?
            .map(|ids| normalize_saved_movie_ids(&ids, &data.movies))
            .unwrap_or_default();
        Ok(data)
    }

    pub fn save(&mut self, data: &LibraryData) -> Result<(), String> {
        let saved_movie_ids = normalize_saved_movie_ids(&data.saved_movie_ids, &data.movies);
        let saved_movie_ids =
            serde_json::to_string(&saved_movie_ids).map_err(|error| error.to_string())?;
        let transaction = self
            .connection
            .transaction()
            .map_err(|error| error.to_string())?;
        for (kind, media) in [("audio", &data.tracks), ("video", &data.movies)] {
            for item in media {
                let (path, encoding) = encode_path(&item.path);
                transaction.execute("INSERT INTO media(id,kind,path,encoding,title,format) VALUES(?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET kind=excluded.kind,path=excluded.path,encoding=excluded.encoding,title=excluded.title,format=excluded.format",
                    params![item.id, kind, path, encoding, item.title, item.format]).map_err(|error| error.to_string())?;
            }
        }
        transaction
            .execute("DELETE FROM playlist_items", [])
            .map_err(|error| error.to_string())?;
        transaction
            .execute("DELETE FROM playlists", [])
            .map_err(|error| error.to_string())?;
        for playlist in &data.playlists {
            transaction
                .execute(
                    "INSERT INTO playlists(id,name) VALUES(?,?)",
                    params![playlist.id, playlist.name],
                )
                .map_err(|error| error.to_string())?;
            for (position, id) in playlist.track_ids.iter().enumerate() {
                transaction
                    .execute(
                        "INSERT INTO playlist_items(playlist_id,position,media_id) VALUES(?,?,?)",
                        params![playlist.id, position as i64, id],
                    )
                    .map_err(|error| error.to_string())?;
            }
        }
        for progress in &data.progress {
            transaction.execute("INSERT INTO progress(media_id,position_ms,duration_ms,completed) VALUES(?,?,?,?) ON CONFLICT(media_id) DO UPDATE SET position_ms=excluded.position_ms,duration_ms=excluded.duration_ms,completed=excluded.completed",
                params![progress.media_id, progress.position_ms.max(0), progress.duration_ms.max(0), progress.completed]).map_err(|error| error.to_string())?;
        }
        for (id, details) in &data.details {
            let encoded = details.artwork.as_ref().map(|path| encode_path(path));
            transaction.execute("INSERT INTO details(media_id,artist,album,album_artist,genre,artwork,art_encoding,art_override,disc_number,track_number) VALUES(?,?,?,?,?,?,?,?,?,?) ON CONFLICT(media_id) DO UPDATE SET artist=excluded.artist,album=excluded.album,album_artist=excluded.album_artist,genre=excluded.genre,artwork=excluded.artwork,art_encoding=excluded.art_encoding,art_override=excluded.art_override,disc_number=excluded.disc_number,track_number=excluded.track_number",
                params![id,details.artist,details.album,details.album_artist,details.genre,encoded.as_ref().map(|(bytes,_)| bytes),encoded.as_ref().map(|(_,encoding)| encoding),details.artwork_override,details.disc_number,details.track_number]).map_err(|error| error.to_string())?;
        }
        for (key, value) in [
            ("view", data.current_view.clone()),
            (
                "selected_playlist",
                data.selected_playlist
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
            ),
            ("volume", data.volume.to_string()),
            ("rate", data.rate.to_string()),
            ("saved_movie_ids", saved_movie_ids),
        ] {
            transaction.execute("INSERT INTO preferences(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key,value]).map_err(|error| error.to_string())?;
        }
        transaction.commit().map_err(|error| error.to_string())
    }
}

fn normalize_saved_movie_ids(ids: &[i32], movies: &[Track]) -> Vec<i32> {
    let movie_ids: HashSet<i32> = movies.iter().map(|movie| movie.id).collect();
    let mut seen = HashSet::new();
    ids.iter()
        .copied()
        .filter(|id| movie_ids.contains(id) && seen.insert(*id))
        .collect()
}

fn backup_before_migration(connection: &Connection, path: &Path) -> Result<(), String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let backup = path.with_extension(format!("before-v{SCHEMA_VERSION}-{stamp}.sqlite3"));
    connection
        .backup(MAIN_DB, backup, None)
        .map_err(|error| error.to_string())
}

enum Command {
    Wake,
    Shutdown,
}

pub struct StorageWorker {
    sender: mpsc::SyncSender<Command>,
    pending: Arc<Mutex<Option<LibraryData>>>,
    error: Arc<Mutex<Option<String>>>,
    join: Option<JoinHandle<()>>,
}

impl StorageWorker {
    pub fn spawn(path: PathBuf) -> Result<(Self, LibraryData), String> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let pending = Arc::new(Mutex::new(None::<LibraryData>));
        let error = Arc::new(Mutex::new(None));
        let worker_pending = Arc::clone(&pending);
        let worker_error = Arc::clone(&error);
        let join = thread::Builder::new()
            .name("library-storage".into())
            .spawn(move || {
                let loaded = LibraryStore::open(&path)
                    .and_then(|store| store.load().map(|data| (store, data)));
                let (mut store, data) = match loaded {
                    Ok(loaded) => loaded,
                    Err(error) => {
                        let _ = ready_sender.send(Err(error));
                        return;
                    }
                };
                if ready_sender.send(Ok(data)).is_err() {
                    return;
                }
                while let Ok(command) = receiver.recv() {
                    let data = worker_pending
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take();
                    if let Some(data) = data {
                        if let Err(message) = store.save(&data) {
                            *worker_error
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(message);
                        }
                    }
                    if matches!(command, Command::Shutdown) {
                        break;
                    }
                }
            })
            .map_err(|error| error.to_string())?;
        match ready_receiver.recv().map_err(|error| error.to_string())? {
            Ok(data) => Ok((
                Self {
                    sender,
                    pending,
                    error,
                    join: Some(join),
                },
                data,
            )),
            Err(error) => {
                let _ = join.join();
                Err(error)
            }
        }
    }

    pub fn save(&self, data: LibraryData) -> Result<(), String> {
        *self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(data);
        match self.sender.try_send(Command::Wake) {
            Ok(()) | Err(mpsc::TrySendError::Full(_)) => Ok(()),
            Err(mpsc::TrySendError::Disconnected(_)) => {
                Err("The library storage worker stopped; changes are still in memory.".into())
            }
        }
    }

    pub fn take_error(&self) -> Option<String> {
        self.error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    pub fn shutdown(&mut self) {
        if let Some(join) = self.join.take() {
            let _ = self.sender.send(Command::Shutdown);
            let _ = join.join();
        }
    }
}

impl Drop for StorageWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(unix)]
fn encode_path(path: &Path) -> (Vec<u8>, &'static str) {
    use std::os::unix::ffi::OsStrExt;
    (path.as_os_str().as_bytes().to_vec(), "unix")
}
#[cfg(windows)]
fn encode_path(path: &Path) -> (Vec<u8>, &'static str) {
    use std::os::windows::ffi::OsStrExt;
    (
        path.as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect(),
        "windows",
    )
}
#[cfg(unix)]
fn decode_path(bytes: &[u8], encoding: &str) -> Result<PathBuf, String> {
    use std::os::unix::ffi::OsStringExt;
    if encoding != "unix" {
        return Err(
            "This library contains paths from a different operating system. It was preserved."
                .into(),
        );
    }
    Ok(std::ffi::OsString::from_vec(bytes.to_vec()).into())
}
#[cfg(windows)]
fn decode_path(bytes: &[u8], encoding: &str) -> Result<PathBuf, String> {
    use std::os::windows::ffi::OsStringExt;
    if encoding != "windows" || !bytes.len().is_multiple_of(2) {
        return Err("This library contains incompatible native paths. It was preserved.".into());
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    Ok(std::ffi::OsString::from_wide(&units).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "rust-vlc-{}-{}.sqlite3",
            std::process::id(),
            fastrand::u64(..)
        ))
    }

    #[test]
    fn reopen_preserves_library_playlist_order_and_resume() {
        let path = test_path();
        let mut store = LibraryStore::open(&path).unwrap();
        let data = LibraryData {
            tracks: vec![Track {
                id: 1,
                path: PathBuf::from("音楽 track.flac"),
                title: "音楽".into(),
                format: "FLAC".into(),
            }],
            playlists: vec![Playlist {
                id: 7,
                name: "Evening".into(),
                track_ids: vec![1, 1],
            }],
            selected_playlist: Some(7),
            progress: vec![Progress {
                media_id: 1,
                position_ms: 22_000,
                duration_ms: 80_000,
                completed: false,
            }],
            details: vec![(
                1,
                MediaDetails {
                    artist: "Track Artist".into(),
                    album: "Collection".into(),
                    album_artist: "Various Artists".into(),
                    genre: "Jazz".into(),
                    track_number: Some(4),
                    disc_number: Some(2),
                    artwork: Some(PathBuf::from("cache/artwork.png")),
                    artwork_override: true,
                },
            )],
            ..LibraryData::default()
        };
        store.save(&data).unwrap();
        drop(store);
        let store = LibraryStore::open(&path).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.tracks, data.tracks);
        assert_eq!(loaded.playlists, data.playlists);
        assert_eq!(loaded.progress, data.progress);
        assert_eq!(loaded.details, data.details);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn saved_movies_reopen_with_duplicate_and_stale_ids_removed() {
        let path = test_path();
        let mut store = LibraryStore::open(&path).unwrap();
        let data = LibraryData {
            movies: vec![
                Track {
                    id: 2,
                    path: PathBuf::from("movie-two.mkv"),
                    title: "Movie Two".into(),
                    format: "MKV".into(),
                },
                Track {
                    id: 3,
                    path: PathBuf::from("movie-three.mkv"),
                    title: "Movie Three".into(),
                    format: "MKV".into(),
                },
            ],
            saved_movie_ids: vec![3, 3, 999, 2],
            ..LibraryData::default()
        };
        store.save(&data).unwrap();
        let stored: String = store
            .connection
            .query_row(
                "SELECT value FROM preferences WHERE key='saved_movie_ids'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, "[3,2]");
        store
            .connection
            .execute(
                "UPDATE preferences SET value='[3,3,999,2]' WHERE key='saved_movie_ids'",
                [],
            )
            .unwrap();
        drop(store);

        let store = LibraryStore::open(&path).unwrap();
        assert_eq!(store.load().unwrap().saved_movie_ids, vec![3, 2]);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn malformed_saved_movie_ids_fail_load_without_overwriting_preference() {
        let path = test_path();
        let store = LibraryStore::open(&path).unwrap();
        store
            .connection
            .execute(
                "INSERT INTO preferences(key,value) VALUES('saved_movie_ids','not-json')",
                [],
            )
            .unwrap();
        let error = store.load().unwrap_err();
        assert!(error.contains("Saved movie IDs"));
        assert!(error.contains("database was preserved"));
        let stored: String = store
            .connection
            .query_row(
                "SELECT value FROM preferences WHERE key='saved_movie_ids'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, "not-json");
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn blob_saved_movie_ids_fail_load_without_overwriting_preference() {
        let path = test_path();
        let store = LibraryStore::open(&path).unwrap();
        store
            .connection
            .execute(
                "INSERT INTO preferences(key,value) VALUES('saved_movie_ids',X'5b315d')",
                [],
            )
            .unwrap();
        let error = store.load().unwrap_err();
        assert!(error.contains("Saved movie IDs"));
        assert!(error.contains("database was preserved"));
        let (kind, bytes): (String, Vec<u8>) = store
            .connection
            .query_row(
                "SELECT typeof(value), value FROM preferences WHERE key='saved_movie_ids'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(kind, "blob");
        assert_eq!(bytes, b"[1]");
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn version_one_details_migrate_album_artist_without_losing_other_fields() {
        let path = test_path();
        let mut store = LibraryStore::open(&path).unwrap();
        let data = LibraryData {
            tracks: vec![Track {
                id: 1,
                path: PathBuf::from("track.mp3"),
                title: "Track".into(),
                format: "MP3".into(),
            }],
            details: vec![(
                1,
                MediaDetails {
                    artist: "Singer".into(),
                    album: "Compilation".into(),
                    album_artist: "Various Artists".into(),
                    genre: "Jazz".into(),
                    track_number: Some(3),
                    disc_number: Some(1),
                    ..MediaDetails::default()
                },
            )],
            ..LibraryData::default()
        };
        store.save(&data).unwrap();
        store
            .connection
            .execute_batch("ALTER TABLE details DROP COLUMN album_artist; PRAGMA user_version=1;")
            .unwrap();
        drop(store);

        let migrated = LibraryStore::open(&path).unwrap();
        let loaded = migrated.load().unwrap();
        let details = &loaded.details[0].1;
        assert_eq!(details.artist, "Singer");
        assert_eq!(details.album, "Compilation");
        assert_eq!(details.album_artist, "");
        assert_eq!(details.genre, "Jazz");
        assert_eq!(details.track_number, Some(3));
        assert_eq!(details.disc_number, Some(1));
        let prefix = format!("{}.before-v2-", path.file_stem().unwrap().to_string_lossy());
        let backups: Vec<PathBuf> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|candidate| {
                candidate
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(&prefix)
            })
            .collect();
        assert_eq!(
            backups.len(),
            1,
            "an upgrade must first preserve its original database"
        );
        let backup = Connection::open(&backups[0]).unwrap();
        assert_eq!(
            backup
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(backup);
        drop(migrated);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_file(&backups[0]).unwrap();
    }

    #[test]
    fn failed_save_rolls_back_and_future_schema_is_preserved() {
        let path = test_path();
        let mut store = LibraryStore::open(&path).unwrap();
        let mut data = LibraryData {
            playlists: vec![Playlist {
                id: 1,
                name: "Valid".into(),
                track_ids: vec![],
            }],
            ..LibraryData::default()
        };
        store.save(&data).unwrap();
        data.playlists[0].track_ids.push(999);
        assert!(store.save(&data).is_err());
        assert!(store.load().unwrap().playlists[0].track_ids.is_empty());
        store
            .connection
            .pragma_update(None, "user_version", 99)
            .unwrap();
        drop(store);
        let before = std::fs::read(&path).unwrap();
        assert!(LibraryStore::open(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn native_path_round_trips_without_lossy_text() {
        #[cfg(windows)]
        let path = {
            use std::os::windows::ffi::OsStringExt;
            PathBuf::from(std::ffi::OsString::from_wide(&[
                67, 58, 92, 0xd800, 32, 0x97f3,
            ]))
        };
        #[cfg(unix)]
        let path = {
            use std::os::unix::ffi::OsStringExt;
            PathBuf::from(std::ffi::OsString::from_vec(vec![b'/', 0xff, b' ', b'x']))
        };
        let (bytes, encoding) = encode_path(&path);
        assert_eq!(decode_path(&bytes, encoding).unwrap(), path);
    }
}
