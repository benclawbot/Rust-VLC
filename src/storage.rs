use crate::model::{MediaDetails, NetworkUrl, Track, TrackSource};
use crate::playback::queue::{QueueCheckpoint, RepeatMode};
use crate::playlists::Playlist;
use rusqlite::{params, Connection, OptionalExtension, MAIN_DB};
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const SCHEMA_VERSION: i64 = 4;
const MAX_BOOKMARKS: usize = 500;
const MAX_NETWORK_HISTORY: usize = 100;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Bookmark {
    pub id: i64,
    pub media_id: i32,
    pub name: String,
    pub position_ms: i64,
}

/// Network addresses remain local to the library database for replay. UI labels should use
/// `url.display_label()` so signed query parameters are never exposed in visible text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkHistoryEntry {
    pub url: NetworkUrl,
    pub title: String,
    pub last_played_at: i64,
}

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
    pub watched_roots: Vec<PathBuf>,
    pub queue_checkpoint: QueueCheckpoint,
    pub bookmarks: Vec<Bookmark>,
    pub network_history: Vec<NetworkHistoryEntry>,
    pub equalizer_settings: crate::playback::EqualizerSettings,
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
            watched_roots: Vec::new(),
            queue_checkpoint: QueueCheckpoint::default(),
            bookmarks: Vec::new(),
            network_history: Vec::new(),
            equalizer_settings: crate::playback::EqualizerSettings::default(),
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

#[derive(Debug)]
pub struct LibraryLock {
    _file: File,
}

impl LibraryLock {
    pub fn try_acquire(database_path: &Path) -> Result<Self, String> {
        let mut lock_path = database_path.as_os_str().to_os_string();
        lock_path.push(".lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(PathBuf::from(lock_path))
            .map_err(|error| {
                format!(
                    "Could not create the library lock ({error}). The database was not opened; check application data folder permissions."
                )
            })?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) => Err(
                "Another Rust-VLC instance is already using this library. Close it before launching Rust-VLC again."
                    .into(),
            ),
            Err(std::fs::TryLockError::Error(error)) => Err(format!(
                "Could not secure exclusive use of the library ({error}). The database was not opened; check application data folder permissions."
            )),
        }
    }
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
                backup_before_migration(&connection, path, SCHEMA_VERSION)?;
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
                 CREATE TABLE watched_roots (position INTEGER PRIMARY KEY, path BLOB NOT NULL, encoding TEXT NOT NULL, UNIQUE(encoding,path));
                 CREATE TABLE bookmarks (id INTEGER PRIMARY KEY, media_id INTEGER NOT NULL REFERENCES media(id), name TEXT NOT NULL, position_ms INTEGER NOT NULL CHECK(position_ms>=0));
                 CREATE INDEX bookmarks_media_idx ON bookmarks(media_id);
                 CREATE TABLE network_history (position INTEGER PRIMARY KEY, url TEXT NOT NULL UNIQUE, title TEXT NOT NULL, last_played_at INTEGER NOT NULL CHECK(last_played_at>=0));
                 PRAGMA user_version=4;"
            ).map_err(|error| error.to_string())?;
            transaction.commit().map_err(|error| error.to_string())?;
        } else {
            let mut migration_version = version;
            if migration_version == 1 {
                backup_before_migration(&connection, path, 2)?;
                let transaction = connection
                    .transaction()
                    .map_err(|error| error.to_string())?;
                transaction
                    .execute_batch(
                        "ALTER TABLE details ADD COLUMN album_artist TEXT NOT NULL DEFAULT ''; PRAGMA user_version=2;",
                    )
                    .map_err(|error| error.to_string())?;
                transaction.commit().map_err(|error| error.to_string())?;
                migration_version = 2;
            }
            if migration_version == 2 {
                backup_before_migration(&connection, path, 3)?;
                let transaction = connection
                    .transaction()
                    .map_err(|error| error.to_string())?;
                transaction
                    .execute_batch(
                        "CREATE TABLE watched_roots (position INTEGER PRIMARY KEY, path BLOB NOT NULL, encoding TEXT NOT NULL, UNIQUE(encoding,path)); PRAGMA user_version=3;",
                    )
                    .map_err(|error| error.to_string())?;
                transaction.commit().map_err(|error| error.to_string())?;
                migration_version = 3;
            }
            if migration_version == 3 {
                backup_before_migration(&connection, path, SCHEMA_VERSION)?;
                let transaction = connection
                    .transaction()
                    .map_err(|error| error.to_string())?;
                transaction
                    .execute_batch(
                        "CREATE TABLE bookmarks (id INTEGER PRIMARY KEY, media_id INTEGER NOT NULL REFERENCES media(id), name TEXT NOT NULL, position_ms INTEGER NOT NULL CHECK(position_ms>=0)); CREATE INDEX bookmarks_media_idx ON bookmarks(media_id); CREATE TABLE network_history (position INTEGER PRIMARY KEY, url TEXT NOT NULL UNIQUE, title TEXT NOT NULL, last_played_at INTEGER NOT NULL CHECK(last_played_at>=0)); PRAGMA user_version=4;",
                    )
                    .map_err(|error| error.to_string())?;
                transaction.commit().map_err(|error| error.to_string())?;
            }
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
            let (path, source) = decode_media_source(&bytes, &encoding)?;
            let track = Track {
                id,
                path,
                title,
                format,
                source,
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

        let mut statement = self
            .connection
            .prepare("SELECT path,encoding FROM watched_roots ORDER BY position")
            .map_err(|error| error.to_string())?;
        let roots = statement
            .query_map([], |row| {
                Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        data.watched_roots = roots
            .iter()
            .map(|(bytes, encoding)| decode_path(bytes, encoding))
            .collect::<Result<_, _>>()?;

        let queue_checkpoint = self
            .connection
            .query_row(
                "SELECT value FROM preferences WHERE key='queue_checkpoint'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| {
                format!(
                    "Playback queue checkpoint could not be read ({error}); the database was preserved."
                )
            })?;
        if let Some(value) = queue_checkpoint {
            let media_ids = data
                .tracks
                .iter()
                .chain(&data.movies)
                .map(|track| track.id)
                .collect::<HashSet<_>>();
            data.queue_checkpoint = parse_queue_checkpoint(&value, &media_ids)?;
        }

        let media_ids = data
            .tracks
            .iter()
            .chain(&data.movies)
            .map(|track| track.id)
            .collect::<HashSet<_>>();
        let mut statement = self
            .connection
            .prepare("SELECT id,media_id,name,position_ms FROM bookmarks ORDER BY id LIMIT ?")
            .map_err(|error| error.to_string())?;
        let bookmarks = statement
            .query_map([MAX_BOOKMARKS as i64 + 1], |row| {
                Ok(Bookmark {
                    id: row.get(0)?,
                    media_id: row.get(1)?,
                    name: row.get(2)?,
                    position_ms: row.get(3)?,
                })
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        data.bookmarks = validate_bookmarks(bookmarks, &media_ids)?;

        let mut statement = self
            .connection
            .prepare(
                "SELECT url,title,last_played_at FROM network_history ORDER BY position LIMIT ?",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([MAX_NETWORK_HISTORY as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(|error| error.to_string())?;
        data.network_history = rows
            .map(|row| {
                let (url, title, last_played_at) = row.map_err(|error| error.to_string())?;
                let url = NetworkUrl::parse(&url).map_err(|_| {
                    "Network stream history contains an invalid URL; the database was preserved."
                        .to_owned()
                })?;
                Ok(NetworkHistoryEntry {
                    url,
                    title,
                    last_played_at,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        data.network_history = validate_network_history(data.network_history)?;

        let equalizer = self
            .connection
            .query_row(
                "SELECT value FROM preferences WHERE key='equalizer_settings'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        if let Some(value) = equalizer.filter(|value| value.len() <= 1024) {
            data.equalizer_settings = parse_equalizer_settings(&value).unwrap_or_default();
        }
        Ok(data)
    }

    pub fn save(&mut self, data: &LibraryData) -> Result<(), String> {
        let saved_movie_ids = normalize_saved_movie_ids(&data.saved_movie_ids, &data.movies);
        let saved_movie_ids =
            serde_json::to_string(&saved_movie_ids).map_err(|error| error.to_string())?;
        let media_ids = data
            .tracks
            .iter()
            .chain(&data.movies)
            .map(|track| track.id)
            .collect::<HashSet<_>>();
        let queue_checkpoint = serialize_queue_checkpoint(&data.queue_checkpoint, &media_ids)?;
        let bookmarks = validate_bookmarks(data.bookmarks.clone(), &media_ids)?;
        let network_history = validate_network_history(data.network_history.clone())?;
        let equalizer_settings = serialize_equalizer_settings(&data.equalizer_settings)?;
        let transaction = self
            .connection
            .transaction()
            .map_err(|error| error.to_string())?;
        for (kind, media) in [("audio", &data.tracks), ("video", &data.movies)] {
            for item in media {
                let (path, encoding) = encode_media_source(item)?;
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
        transaction
            .execute("DELETE FROM bookmarks", [])
            .map_err(|error| error.to_string())?;
        for bookmark in &bookmarks {
            transaction
                .execute(
                    "INSERT INTO bookmarks(id,media_id,name,position_ms) VALUES(?,?,?,?)",
                    params![
                        bookmark.id,
                        bookmark.media_id,
                        bookmark.name,
                        bookmark.position_ms
                    ],
                )
                .map_err(|error| error.to_string())?;
        }
        transaction
            .execute("DELETE FROM network_history", [])
            .map_err(|error| error.to_string())?;
        for (position, item) in network_history.iter().enumerate() {
            transaction
                .execute(
                    "INSERT INTO network_history(position,url,title,last_played_at) VALUES(?,?,?,?)",
                    params![position as i64, item.url.as_str(), item.title, item.last_played_at],
                )
                .map_err(|error| error.to_string())?;
        }
        transaction
            .execute("DELETE FROM watched_roots", [])
            .map_err(|error| error.to_string())?;
        for (position, root) in data.watched_roots.iter().enumerate() {
            let (path, encoding) = encode_path(root);
            transaction
                .execute(
                    "INSERT INTO watched_roots(position,path,encoding) VALUES(?,?,?)",
                    params![position as i64, path, encoding],
                )
                .map_err(|error| error.to_string())?;
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
            ("queue_checkpoint", queue_checkpoint),
            ("equalizer_settings", equalizer_settings),
        ] {
            transaction.execute("INSERT INTO preferences(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key,value]).map_err(|error| error.to_string())?;
        }
        transaction.commit().map_err(|error| error.to_string())
    }
}

fn encode_media_source(track: &Track) -> Result<(Vec<u8>, &'static str), String> {
    match &track.source {
        TrackSource::Local => {
            if track.path.as_os_str().is_empty() {
                return Err("A local media item must have a native path.".into());
            }
            let (bytes, encoding) = encode_path(&track.path);
            Ok((bytes, encoding))
        }
        TrackSource::Network(url) => {
            if !track.path.as_os_str().is_empty() {
                return Err("A network media item cannot also contain a native path.".into());
            }
            let url = url.for_persistence();
            Ok((url.as_str().as_bytes().to_vec(), "url"))
        }
    }
}

fn decode_media_source(bytes: &[u8], encoding: &str) -> Result<(PathBuf, TrackSource), String> {
    if encoding == "url" {
        let url = std::str::from_utf8(bytes).map_err(|_| {
            "The library contains an invalid network URL; the database was preserved.".to_owned()
        })?;
        let url = NetworkUrl::parse(url).map_err(|_| {
            "The library contains an invalid network URL; the database was preserved.".to_owned()
        })?;
        return Ok((PathBuf::new(), TrackSource::Network(url)));
    }
    if !matches!(encoding, "unix" | "windows") {
        return Err(
            "The library contains an unknown media path encoding. It was preserved.".into(),
        );
    }
    Ok((decode_path(bytes, encoding)?, TrackSource::Local))
}

fn validate_bookmarks(
    bookmarks: Vec<Bookmark>,
    media_ids: &HashSet<i32>,
) -> Result<Vec<Bookmark>, String> {
    if bookmarks.len() > MAX_BOOKMARKS {
        return Err(format!(
            "The library has more than {MAX_BOOKMARKS} bookmarks."
        ));
    }
    let mut seen_ids = HashSet::new();
    let mut normalized = Vec::with_capacity(bookmarks.len());
    for mut bookmark in bookmarks {
        let name = bookmark.name.trim();
        if bookmark.id <= 0
            || !seen_ids.insert(bookmark.id)
            || !media_ids.contains(&bookmark.media_id)
            || bookmark.position_ms < 0
            || name.is_empty()
            || name.len() > 128
            || name.chars().any(char::is_control)
        {
            return Err(
                "A saved bookmark is invalid or references unknown media; the database was preserved."
                    .into(),
            );
        }
        bookmark.name = name.to_owned();
        normalized.push(bookmark);
    }
    Ok(normalized)
}

fn validate_network_history(
    history: Vec<NetworkHistoryEntry>,
) -> Result<Vec<NetworkHistoryEntry>, String> {
    let mut seen_urls = HashSet::new();
    let mut normalized = Vec::with_capacity(history.len().min(MAX_NETWORK_HISTORY));
    for mut item in history.into_iter().take(MAX_NETWORK_HISTORY) {
        if item.last_played_at < 0 {
            return Err("Network stream history contains an invalid timestamp.".into());
        }
        let url = item.url.for_persistence();
        let key = url.as_str().to_owned();
        if !seen_urls.insert(key) {
            continue;
        }
        let title = item.title.trim();
        if title.len() > 256 || title.chars().any(char::is_control) {
            return Err("Network stream history contains an invalid display title.".into());
        }
        item.url = url;
        item.title = if title.is_empty() || title.contains("://") {
            item.url.display_label()
        } else {
            title.to_owned()
        };
        normalized.push(item);
        if normalized.len() == MAX_NETWORK_HISTORY {
            break;
        }
    }
    Ok(normalized)
}

fn serialize_equalizer_settings(
    settings: &crate::playback::EqualizerSettings,
) -> Result<String, String> {
    if !valid_equalizer_settings(settings) {
        return Err("Equalizer settings contain an invalid gain or preset.".into());
    }
    serde_json::to_string(&(
        settings.enabled,
        settings.preamp_db,
        settings.bands_db,
        settings.preset_index,
    ))
    .map_err(|_| "Equalizer settings could not be saved.".into())
}

fn parse_equalizer_settings(value: &str) -> Option<crate::playback::EqualizerSettings> {
    let (enabled, preamp_db, bands_db, preset_index): (bool, f32, [f32; 10], Option<usize>) =
        serde_json::from_str(value).ok()?;
    let settings = crate::playback::EqualizerSettings {
        enabled,
        preamp_db,
        bands_db,
        preset_index,
    };
    valid_equalizer_settings(&settings).then_some(settings)
}

fn valid_equalizer_settings(settings: &crate::playback::EqualizerSettings) -> bool {
    let valid_gain = |gain: f32| gain.is_finite() && (-20.0..=20.0).contains(&gain);
    valid_gain(settings.preamp_db)
        && settings.bands_db.into_iter().all(valid_gain)
        && settings.preset_index.is_none_or(|index| index <= 255)
}

fn serialize_queue_checkpoint(
    checkpoint: &QueueCheckpoint,
    media_ids: &HashSet<i32>,
) -> Result<String, String> {
    if !checkpoint.is_valid()
        || checkpoint
            .entry_ids
            .iter()
            .any(|id| !media_ids.contains(id))
    {
        return Err("Playback queue checkpoint is invalid or references unknown media.".into());
    }
    let repeat_mode = match checkpoint.repeat_mode {
        RepeatMode::Off => "off",
        RepeatMode::One => "one",
        RepeatMode::All => "all",
    };
    serde_json::to_string(&(
        &checkpoint.entry_ids,
        &checkpoint.order,
        checkpoint.position,
        checkpoint.wrapped_from_end,
        repeat_mode,
        checkpoint.shuffle,
        checkpoint.position_ms,
    ))
    .map_err(|error| error.to_string())
}

fn parse_queue_checkpoint(
    value: &str,
    media_ids: &HashSet<i32>,
) -> Result<QueueCheckpoint, String> {
    let (entry_ids, order, position, wrapped_from_end, repeat, shuffle, position_ms): (
        Vec<i32>,
        Vec<usize>,
        Option<usize>,
        bool,
        String,
        bool,
        i64,
    ) = serde_json::from_str(value).map_err(|error| {
        format!("Playback queue checkpoint is invalid ({error}); the database was preserved.")
    })?;
    let repeat_mode =
        match repeat.as_str() {
            "off" => RepeatMode::Off,
            "one" => RepeatMode::One,
            "all" => RepeatMode::All,
            _ => return Err(
                "Playback queue checkpoint has an unknown repeat mode; the database was preserved."
                    .into(),
            ),
        };
    let checkpoint = QueueCheckpoint {
        entry_ids,
        order,
        position,
        wrapped_from_end,
        repeat_mode,
        shuffle,
        position_ms,
    };
    if !checkpoint.is_valid()
        || checkpoint
            .entry_ids
            .iter()
            .any(|id| !media_ids.contains(id))
    {
        return Err(
            "Playback queue checkpoint is invalid or references unknown media; the database was preserved."
                .into(),
        );
    }
    Ok(checkpoint)
}

fn normalize_saved_movie_ids(ids: &[i32], movies: &[Track]) -> Vec<i32> {
    let movie_ids: HashSet<i32> = movies.iter().map(|movie| movie.id).collect();
    let mut seen = HashSet::new();
    ids.iter()
        .copied()
        .filter(|id| movie_ids.contains(id) && seen.insert(*id))
        .collect()
}

fn backup_before_migration(
    connection: &Connection,
    path: &Path,
    target_version: i64,
) -> Result<(), String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let backup = path.with_extension(format!("before-v{target_version}-{stamp}.sqlite3"));
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
    use std::process::{Child, Command, Stdio};

    struct KillOnDrop(Child);

    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn test_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "rust-vlc-{}-{}.sqlite3",
            std::process::id(),
            fastrand::u64(..)
        ))
    }

    #[test]
    fn library_lock_process_helper() {
        if std::env::var_os("RUST_VLC_LIBRARY_LOCK_HELPER").is_none() {
            return;
        }
        let (Some(database), Some(ready)) = (
            std::env::var_os("RUST_VLC_LIBRARY_LOCK_PATH"),
            std::env::var_os("RUST_VLC_LIBRARY_LOCK_READY"),
        ) else {
            return;
        };
        let _lock = LibraryLock::try_acquire(&PathBuf::from(database)).unwrap();
        std::fs::write(ready, b"held").unwrap();
        std::thread::sleep(Duration::from_secs(60));
    }

    #[test]
    fn library_lock_blocks_another_process_and_recovers_after_termination() {
        let database = test_path();
        let mut ready = database.as_os_str().to_os_string();
        ready.push(".ready");
        let ready = PathBuf::from(ready);
        let mut child = KillOnDrop(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "storage::tests::library_lock_process_helper",
                    "--nocapture",
                ])
                .env("RUST_VLC_LIBRARY_LOCK_HELPER", "1")
                .env("RUST_VLC_LIBRARY_LOCK_PATH", &database)
                .env("RUST_VLC_LIBRARY_LOCK_READY", &ready)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !ready.exists() {
            if let Some(status) = child.0.try_wait().unwrap() {
                panic!("library lock process exited before acquiring its lock: {status}");
            }
            assert!(
                std::time::Instant::now() < deadline,
                "library lock process did not acquire its lock in time"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        let error = LibraryLock::try_acquire(&database).unwrap_err();
        assert!(error.contains("already using this library"), "{error}");
        let mut lock_path = database.as_os_str().to_os_string();
        lock_path.push(".lock");
        let lock_path = PathBuf::from(lock_path);
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        std::fs::remove_file(&ready).unwrap();
        assert!(lock_path.is_file(), "the lock file must not be unlinked");

        let lock = LibraryLock::try_acquire(&database).unwrap();
        drop(lock);
        assert!(
            lock_path.is_file(),
            "dropping the guard must not unlink the lock file"
        );
        LibraryLock::try_acquire(&database).unwrap();
    }

    fn offline_native_root() -> PathBuf {
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            let mut units = "rust-vlc-offline-root-".encode_utf16().collect::<Vec<_>>();
            units.extend(std::process::id().to_string().encode_utf16());
            units.push(0xd800);
            std::env::temp_dir().join(std::ffi::OsString::from_wide(&units))
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let mut bytes = format!("rust-vlc-offline-root-{}-", std::process::id()).into_bytes();
            bytes.push(0xff);
            std::env::temp_dir().join(std::ffi::OsString::from_vec(bytes))
        }
    }

    #[test]
    fn reopen_preserves_library_playlist_order_resume_and_queue_checkpoint() {
        let path = test_path();
        let mut store = LibraryStore::open(&path).unwrap();
        let data = LibraryData {
            tracks: vec![
                Track {
                    id: 1,
                    path: PathBuf::from("音楽 track.flac"),
                    title: "音楽".into(),
                    format: "FLAC".into(),
                    source: TrackSource::Local,
                },
                Track {
                    id: 2,
                    path: PathBuf::from("second track.mp3"),
                    title: "Second".into(),
                    format: "MP3".into(),
                    source: TrackSource::Local,
                },
            ],
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
            watched_roots: vec![
                std::env::temp_dir().join(format!("offline music root {}", fastrand::u64(..)))
            ],
            queue_checkpoint: QueueCheckpoint {
                entry_ids: vec![1, 2, 1],
                order: vec![2, 1, 0],
                position: Some(1),
                wrapped_from_end: false,
                repeat_mode: RepeatMode::All,
                shuffle: true,
                position_ms: 22_000,
            },
            ..LibraryData::default()
        };
        assert!(!data.watched_roots[0].exists());
        store.save(&data).unwrap();
        drop(store);
        let store = LibraryStore::open(&path).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.tracks, data.tracks);
        assert_eq!(loaded.playlists, data.playlists);
        assert_eq!(loaded.progress, data.progress);
        assert_eq!(loaded.details, data.details);
        assert_eq!(loaded.watched_roots, data.watched_roots);
        assert_eq!(loaded.queue_checkpoint, data.queue_checkpoint);
        drop(store);
        std::fs::remove_file(&path).unwrap();
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
                    source: TrackSource::Local,
                },
                Track {
                    id: 3,
                    path: PathBuf::from("movie-three.mkv"),
                    title: "Movie Three".into(),
                    format: "MKV".into(),
                    source: TrackSource::Local,
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
        std::fs::remove_file(&path).unwrap();
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
    fn version_two_migrates_with_backup_and_preserves_existing_library_state() {
        let path = test_path();
        let mut store = LibraryStore::open(&path).unwrap();
        let data = LibraryData {
            tracks: vec![Track {
                id: 1,
                path: PathBuf::from("music/track.flac"),
                title: "Track".into(),
                format: "FLAC".into(),
                source: TrackSource::Local,
            }],
            movies: vec![Track {
                id: 2,
                path: PathBuf::from("movies/movie.mkv"),
                title: "Movie".into(),
                format: "MOVIE".into(),
                source: TrackSource::Local,
            }],
            saved_movie_ids: vec![2],
            playlists: vec![Playlist {
                id: 7,
                name: "Evening".into(),
                track_ids: vec![1, 1],
            }],
            selected_playlist: Some(7),
            current_view: "music".into(),
            volume: 41,
            rate: 1.25,
            progress: vec![Progress {
                media_id: 2,
                position_ms: 22_000,
                duration_ms: 80_000,
                completed: false,
            }],
            details: vec![(
                1,
                MediaDetails {
                    artist: "Singer".into(),
                    album: "Compilation".into(),
                    album_artist: "Various Artists".into(),
                    genre: "Jazz".into(),
                    track_number: Some(4),
                    disc_number: Some(2),
                    ..MediaDetails::default()
                },
            )],
            ..LibraryData::default()
        };
        store.save(&data).unwrap();
        store
            .connection
            .execute_batch(
                "DROP TABLE watched_roots; DROP TABLE network_history; DROP TABLE bookmarks; DELETE FROM preferences WHERE key='queue_checkpoint'; PRAGMA user_version=2;",
            )
            .unwrap();
        drop(store);

        let migrated = LibraryStore::open(&path).unwrap();
        let loaded = migrated.load().unwrap();
        assert_eq!(loaded.tracks, data.tracks);
        assert_eq!(loaded.movies, data.movies);
        assert_eq!(loaded.saved_movie_ids, data.saved_movie_ids);
        assert_eq!(loaded.playlists, data.playlists);
        assert_eq!(loaded.selected_playlist, data.selected_playlist);
        assert_eq!(loaded.current_view, data.current_view);
        assert_eq!(loaded.volume, data.volume);
        assert_eq!(loaded.rate, data.rate);
        assert_eq!(loaded.progress, data.progress);
        assert_eq!(loaded.details, data.details);
        assert!(loaded.watched_roots.is_empty());
        assert_eq!(loaded.queue_checkpoint, QueueCheckpoint::default());
        let prefix = format!("{}.before-v3-", path.file_stem().unwrap().to_string_lossy());
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
        assert_eq!(backups.len(), 1);
        let backup = Connection::open(&backups[0]).unwrap();
        assert_eq!(
            backup
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            backup
                .query_row("SELECT count(*) FROM media", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            2
        );
        drop(backup);
        drop(migrated);
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(&backups[0]).unwrap();
        let prefix_v4 = format!("{}.before-v4-", path.file_stem().unwrap().to_string_lossy());
        for candidate in std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|candidate| {
                candidate
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(&prefix_v4)
            })
        {
            std::fs::remove_file(candidate).unwrap();
        }
    }

    #[test]
    fn version_three_migrates_to_four_with_backup_and_preserves_existing_state() {
        let path = test_path();
        let mut store = LibraryStore::open(&path).unwrap();
        let data = LibraryData {
            tracks: vec![Track {
                id: 1,
                path: PathBuf::from("music/track.flac"),
                title: "Track".into(),
                format: "FLAC".into(),
                source: TrackSource::Local,
            }],
            movies: vec![Track {
                id: 2,
                path: PathBuf::from("movies/movie.mkv"),
                title: "Movie".into(),
                format: "MOVIE".into(),
                source: TrackSource::Local,
            }],
            saved_movie_ids: vec![2],
            playlists: vec![Playlist {
                id: 4,
                name: "Road mix".into(),
                track_ids: vec![1, 1],
            }],
            selected_playlist: Some(4),
            current_view: "music".into(),
            volume: 39,
            rate: 1.25,
            progress: vec![Progress {
                media_id: 1,
                position_ms: 5_000,
                duration_ms: 40_000,
                completed: false,
            }],
            watched_roots: vec![PathBuf::from("offline music root")],
            queue_checkpoint: QueueCheckpoint {
                entry_ids: vec![1, 1],
                order: vec![1, 0],
                position: Some(0),
                wrapped_from_end: true,
                repeat_mode: RepeatMode::All,
                shuffle: true,
                position_ms: 5_000,
            },
            equalizer_settings: crate::playback::EqualizerSettings {
                enabled: true,
                preamp_db: 2.5,
                bands_db: [1.0; 10],
                preset_index: Some(2),
            },
            ..LibraryData::default()
        };
        store.save(&data).unwrap();
        store
            .connection
            .execute_batch(
                "DROP TABLE bookmarks; DROP TABLE network_history; PRAGMA user_version=3;",
            )
            .unwrap();
        drop(store);

        let migrated = LibraryStore::open(&path).unwrap();
        let loaded = migrated.load().unwrap();
        assert_eq!(loaded.tracks, data.tracks);
        assert_eq!(loaded.movies, data.movies);
        assert_eq!(loaded.playlists, data.playlists);
        assert_eq!(loaded.progress, data.progress);
        assert_eq!(loaded.watched_roots, data.watched_roots);
        assert_eq!(loaded.queue_checkpoint, data.queue_checkpoint);
        assert_eq!(loaded.equalizer_settings, data.equalizer_settings);
        assert!(loaded.bookmarks.is_empty());
        assert!(loaded.network_history.is_empty());

        let prefix = format!("{}.before-v4-", path.file_stem().unwrap().to_string_lossy());
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
        assert_eq!(backups.len(), 1);
        let backup = Connection::open(&backups[0]).unwrap();
        assert_eq!(
            backup
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert_eq!(
            backup
                .query_row("SELECT count(*) FROM media", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            2
        );
        drop(backup);
        drop(migrated);
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(&backups[0]).unwrap();
    }

    #[test]
    fn bookmarks_network_history_and_equalizer_reopen_without_losing_signed_urls_or_occurrences() {
        let path = test_path();
        let mut store = LibraryStore::open(&path).unwrap();
        let stream_url = format!(
            "https://radio.example.test/live?token=secret&signature=valid&payload={}#metadata",
            "é".repeat(660)
        );
        let stream = Track::from_network_audio_url(41, &stream_url).unwrap();
        let canonical_stream_url = match &stream.source {
            TrackSource::Network(url) => url.as_str().to_owned(),
            TrackSource::Local => unreachable!(),
        };
        assert!(stream_url.len() < 4096);
        assert!(canonical_stream_url.len() <= 4096);
        let mut data = LibraryData {
            tracks: vec![stream.clone()],
            bookmarks: vec![Bookmark {
                id: 8,
                media_id: 41,
                name: "Verse two".into(),
                position_ms: 73_000,
            }],
            network_history: vec![NetworkHistoryEntry {
                url: match &stream.source {
                    TrackSource::Network(url) => url.clone(),
                    TrackSource::Local => unreachable!(),
                },
                title: "Evening radio".into(),
                last_played_at: 1_800_000_000,
            }],
            queue_checkpoint: QueueCheckpoint {
                entry_ids: vec![41, 41],
                order: vec![1, 0],
                position: Some(1),
                wrapped_from_end: false,
                repeat_mode: RepeatMode::One,
                shuffle: true,
                position_ms: 73_000,
            },
            equalizer_settings: crate::playback::EqualizerSettings {
                enabled: true,
                preamp_db: -4.5,
                bands_db: [3.25; 10],
                preset_index: Some(1),
            },
            ..LibraryData::default()
        };
        data.network_history.push(data.network_history[0].clone());
        store.save(&data).unwrap();
        drop(store);

        let store = LibraryStore::open(&path).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.tracks.as_slice(), std::slice::from_ref(&stream));
        assert!(loaded.tracks[0].path.as_os_str().is_empty());
        assert_eq!(loaded.bookmarks, data.bookmarks);
        assert_eq!(loaded.network_history.len(), 1);
        assert_eq!(loaded.network_history[0].title, "Evening radio");
        assert_eq!(loaded.network_history[0].url.as_str(), canonical_stream_url);
        assert_eq!(loaded.queue_checkpoint, data.queue_checkpoint);
        assert_eq!(loaded.equalizer_settings, data.equalizer_settings);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn invalid_bookmarks_equalizer_and_unknown_media_encoding_are_rejected_safely() {
        let path = test_path();
        let mut store = LibraryStore::open(&path).unwrap();
        let valid = LibraryData {
            tracks: vec![Track {
                id: 1,
                path: PathBuf::from("track.mp3"),
                title: "Track".into(),
                format: "MP3".into(),
                source: TrackSource::Local,
            }],
            ..LibraryData::default()
        };
        store.save(&valid).unwrap();
        let invalid_bookmark = LibraryData {
            bookmarks: vec![Bookmark {
                id: 1,
                media_id: 999,
                name: "Missing media".into(),
                position_ms: 0,
            }],
            ..valid.clone()
        };
        assert!(store
            .save(&invalid_bookmark)
            .unwrap_err()
            .contains("bookmark"));
        let invalid_eq = LibraryData {
            equalizer_settings: crate::playback::EqualizerSettings {
                preamp_db: f32::INFINITY,
                ..crate::playback::EqualizerSettings::default()
            },
            ..valid
        };
        assert!(store.save(&invalid_eq).unwrap_err().contains("Equalizer"));
        store
            .connection
            .execute("UPDATE media SET encoding='future' WHERE id=1", [])
            .unwrap();
        assert!(store
            .load()
            .unwrap_err()
            .contains("unknown media path encoding"));
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn invalid_equalizer_preference_loads_safe_default_without_rewriting_database() {
        let path = test_path();
        let store = LibraryStore::open(&path).unwrap();
        let invalid = "[true,21.0,[0.0,0.0,0.0,0.0,0.0,0.0,0.0,0.0,0.0,0.0],null]";
        store
            .connection
            .execute(
                "INSERT INTO preferences(key,value) VALUES('equalizer_settings',?)",
                [invalid],
            )
            .unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(
            loaded.equalizer_settings,
            crate::playback::EqualizerSettings::default()
        );
        let preserved: String = store
            .connection
            .query_row(
                "SELECT value FROM preferences WHERE key='equalizer_settings'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(preserved, invalid);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn stream_history_is_bounded_and_malformed_userinfo_urls_are_not_exposed() {
        let path = test_path();
        let mut store = LibraryStore::open(&path).unwrap();
        let history = (0..150)
            .map(|index| NetworkHistoryEntry {
                url: NetworkUrl::parse(&format!(
                    "https://radio.example.test/station-{index}?token=private-{index}"
                ))
                .unwrap(),
                title: format!("Station {index}"),
                last_played_at: index,
            })
            .collect::<Vec<_>>();
        store
            .save(&LibraryData {
                network_history: history,
                ..LibraryData::default()
            })
            .unwrap();
        assert_eq!(
            store.load().unwrap().network_history.len(),
            MAX_NETWORK_HISTORY
        );

        store
            .connection
            .execute(
                "INSERT OR REPLACE INTO network_history(position,url,title,last_played_at) VALUES(0,?,?,0)",
                params!["https://user:secret@radio.example.test/live", "Unsafe"],
            )
            .unwrap();
        let error = store.load().unwrap_err();
        assert!(error.contains("invalid URL"));
        assert!(!error.contains("secret"));
        let stored: String = store
            .connection
            .query_row(
                "SELECT url FROM network_history WHERE position=0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, "https://user:secret@radio.example.test/live");
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn malformed_and_unknown_queue_checkpoints_fail_load_without_rewriting_database() {
        let path = test_path();
        let mut store = LibraryStore::open(&path).unwrap();
        store
            .save(&LibraryData {
                tracks: vec![Track {
                    id: 1,
                    path: PathBuf::from("track.mp3"),
                    title: "Track".into(),
                    format: "MP3".into(),
                    source: TrackSource::Local,
                }],
                ..LibraryData::default()
            })
            .unwrap();
        for value in [
            "not-json",
            "[[999],[0],0,false,\"off\",false,1000]",
            "[[1],[0],0,false,\"future\",false,1000]",
        ] {
            store
                .connection
                .execute(
                    "UPDATE preferences SET value=? WHERE key='queue_checkpoint'",
                    [value],
                )
                .unwrap();
            let error = store.load().unwrap_err();
            assert!(error.contains("queue checkpoint"), "{error}");
            let saved: String = store
                .connection
                .query_row(
                    "SELECT value FROM preferences WHERE key='queue_checkpoint'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(saved, value);
        }
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn offline_watched_root_round_trips_as_a_native_path() {
        let path = test_path();
        let root = offline_native_root();
        assert!(!root.exists());
        let mut store = LibraryStore::open(&path).unwrap();
        store
            .save(&LibraryData {
                watched_roots: vec![root.clone()],
                ..LibraryData::default()
            })
            .unwrap();
        drop(store);

        let store = LibraryStore::open(&path).unwrap();
        assert_eq!(
            store.load().unwrap().watched_roots.as_slice(),
            std::slice::from_ref(&root)
        );
        assert!(!root.exists(), "loading a root must not scan or create it");
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn removing_a_watched_root_keeps_imported_media_and_history() {
        let path = test_path();
        let media = Track {
            id: 1,
            path: PathBuf::from("offline-root/track.mp3"),
            title: "Track".into(),
            format: "MP3".into(),
            source: TrackSource::Local,
        };
        let mut store = LibraryStore::open(&path).unwrap();
        let mut data = LibraryData {
            tracks: vec![media.clone()],
            watched_roots: vec![PathBuf::from("offline-root")],
            progress: vec![Progress {
                media_id: 1,
                position_ms: 5_000,
                duration_ms: 20_000,
                completed: false,
            }],
            ..LibraryData::default()
        };
        store.save(&data).unwrap();
        data.watched_roots.clear();
        store.save(&data).unwrap();

        let loaded = store.load().unwrap();
        assert!(loaded.watched_roots.is_empty());
        assert_eq!(loaded.tracks, [media]);
        assert_eq!(loaded.progress, data.progress);
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
                source: TrackSource::Local,
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
            .execute_batch(
                "DROP TABLE watched_roots; DROP TABLE network_history; DROP TABLE bookmarks; DELETE FROM preferences WHERE key='queue_checkpoint'; ALTER TABLE details DROP COLUMN album_artist; PRAGMA user_version=1;",
            )
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
        let prefix_v3 = format!("{}.before-v3-", path.file_stem().unwrap().to_string_lossy());
        let backup = Connection::open(&backups[0]).unwrap();
        assert_eq!(
            backup
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(backup);
        drop(migrated);
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(&backups[0]).unwrap();
        for candidate in std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|candidate| {
                candidate
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(&prefix_v3)
            })
        {
            std::fs::remove_file(candidate).unwrap();
        }
        let prefix_v4 = format!("{}.before-v4-", path.file_stem().unwrap().to_string_lossy());
        for candidate in std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|candidate| {
                candidate
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(&prefix_v4)
            })
        {
            std::fs::remove_file(candidate).unwrap();
        }
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
