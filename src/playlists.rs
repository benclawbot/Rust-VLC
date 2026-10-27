use crate::model::{NetworkUrl, Track, TrackId, TrackSource};
use std::fmt;
use std::path::{Path, PathBuf};

pub type PlaylistId = i32;

const MAX_M3U8_BYTES: usize = 2 * 1024 * 1024;
const MAX_M3U8_ENTRIES: usize = 10_000;
const MAX_M3U8_LINE_BYTES: usize = 8192;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum M3u8EntrySource {
    Local(PathBuf),
    Network(NetworkUrl),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct M3u8Entry {
    pub source: M3u8EntrySource,
    pub title: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum M3u8Error {
    TooLarge,
    InvalidUtf8,
    InvalidLine,
    TooManyEntries,
    InvalidSource,
    UnknownTrack,
    UnrepresentablePath,
}

impl fmt::Display for M3u8Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TooLarge => "The playlist exceeds the 2 MiB import limit.",
            Self::InvalidUtf8 => "The playlist must be UTF-8 encoded.",
            Self::InvalidLine => "The playlist contains an invalid or overlong line.",
            Self::TooManyEntries => "The playlist exceeds the 10,000-entry limit.",
            Self::InvalidSource => "The playlist contains an invalid or unsupported media URL.",
            Self::UnknownTrack => "A playlist entry refers to media that is no longer available.",
            Self::UnrepresentablePath => "A media path cannot be represented in an M3U8 file.",
        })
    }
}

impl std::error::Error for M3u8Error {}

/// Parse an M3U8 playlist while retaining duplicate occurrences and resolving local relative
/// paths against the playlist's containing directory. This function never writes to media.
pub fn parse_m3u8(input: &[u8], playlist_dir: &Path) -> Result<Vec<M3u8Entry>, M3u8Error> {
    if input.len() > MAX_M3U8_BYTES {
        return Err(M3u8Error::TooLarge);
    }
    let text = std::str::from_utf8(input).map_err(|_| M3u8Error::InvalidUtf8)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut title = None;
    let mut entries = Vec::new();
    for raw_line in text.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.len() > MAX_M3U8_LINE_BYTES || line.contains('\0') {
            return Err(M3u8Error::InvalidLine);
        }
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with("#EXTINF:") {
            title = line
                .split_once(',')
                .map(|(_, title)| title.trim())
                .filter(|title| !title.is_empty())
                .map(str::to_owned);
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        if entries.len() == MAX_M3U8_ENTRIES {
            return Err(M3u8Error::TooManyEntries);
        }
        entries.push(M3u8Entry {
            source: parse_m3u8_source(line, playlist_dir)?,
            title: title.take(),
        });
    }
    Ok(entries)
}

fn parse_m3u8_source(line: &str, playlist_dir: &Path) -> Result<M3u8EntrySource, M3u8Error> {
    if let Some(scheme) = uri_scheme(line) {
        if scheme.eq_ignore_ascii_case("file") {
            if !line[scheme.len() + 1..].starts_with("//") {
                return Err(M3u8Error::InvalidSource);
            }
            let url = reqwest::Url::parse(line).map_err(|_| M3u8Error::InvalidSource)?;
            let path = url.to_file_path().map_err(|_| M3u8Error::InvalidSource)?;
            return Ok(M3u8EntrySource::Local(path));
        }
        return NetworkUrl::parse(line)
            .map(M3u8EntrySource::Network)
            .map_err(|_| M3u8Error::InvalidSource);
    }
    let path = PathBuf::from(line);
    Ok(M3u8EntrySource::Local(if path.is_absolute() {
        path
    } else {
        playlist_dir.join(path)
    }))
}

fn uri_scheme(value: &str) -> Option<&str> {
    if value.as_bytes().get(1) == Some(&b':')
        && value
            .as_bytes()
            .get(2)
            .is_some_and(|byte| matches!(*byte, b'/' | b'\\'))
        && value.as_bytes()[0].is_ascii_alphabetic()
    {
        return None;
    }
    let (scheme, _) = value.split_once(':')?;
    (!scheme.is_empty()
        && scheme.as_bytes()[0].is_ascii_alphabetic()
        && scheme
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'.' | b'-')))
    .then_some(scheme)
}

/// Serialize selected playlist entries without touching source-media files. Absolute local paths
/// under `playlist_dir` are made relative; network URLs remain in the playlist for replay.
pub fn export_m3u8(
    playlist: &Playlist,
    tracks: &[Track],
    playlist_dir: &Path,
) -> Result<String, M3u8Error> {
    if playlist.track_ids.len() > MAX_M3U8_ENTRIES {
        return Err(M3u8Error::TooManyEntries);
    }
    let mut output = String::from("#EXTM3U\n");
    for id in &playlist.track_ids {
        let track = tracks
            .iter()
            .find(|track| track.id == *id)
            .ok_or(M3u8Error::UnknownTrack)?;
        let source = match &track.source {
            TrackSource::Network(url) => {
                if !track.path.as_os_str().is_empty() {
                    return Err(M3u8Error::InvalidSource);
                }
                url.as_str().to_owned()
            }
            TrackSource::Local => {
                if track.path.as_os_str().is_empty() {
                    return Err(M3u8Error::InvalidSource);
                }
                let path = if track.path.is_absolute() {
                    track.path.strip_prefix(playlist_dir).unwrap_or(&track.path)
                } else {
                    &track.path
                };
                let path = path.to_str().ok_or(M3u8Error::UnrepresentablePath)?;
                if path.is_empty() {
                    return Err(M3u8Error::InvalidSource);
                }
                if Path::new(path).is_relative() {
                    format!("./{path}")
                } else {
                    path.to_owned()
                }
            }
        };
        if source.len() > MAX_M3U8_LINE_BYTES || source.contains(['\r', '\n', '\0']) {
            return Err(M3u8Error::InvalidLine);
        }
        if track.title.len() > 512 || track.title.chars().any(char::is_control) {
            return Err(M3u8Error::InvalidLine);
        }
        output.push_str("#EXTINF:-1,");
        output.push_str(&track.title);
        output.push('\n');
        output.push_str(&source);
        output.push('\n');
    }
    Ok(output)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Playlist {
    pub id: PlaylistId,
    pub name: String,
    pub track_ids: Vec<TrackId>,
}

#[derive(Debug, Default)]
pub struct PlaylistCollection {
    playlists: Vec<Playlist>,
    selected_id: Option<PlaylistId>,
    next_id: PlaylistId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaylistError {
    EmptyName,
    DuplicateName,
    NoPlaylistSelected,
}

impl fmt::Display for PlaylistError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyName => write!(formatter, "Enter a name for the new playlist."),
            Self::DuplicateName => write!(formatter, "A playlist with that name already exists."),
            Self::NoPlaylistSelected => write!(formatter, "Choose a playlist first."),
        }
    }
}

impl std::error::Error for PlaylistError {}

impl PlaylistCollection {
    pub fn restore(playlists: Vec<Playlist>, selected_id: Option<PlaylistId>) -> Self {
        let next_id = playlists
            .iter()
            .map(|playlist| playlist.id)
            .max()
            .unwrap_or(0);
        let selected_id =
            selected_id.filter(|id| playlists.iter().any(|playlist| playlist.id == *id));
        Self {
            playlists,
            selected_id,
            next_id,
        }
    }

    pub fn playlists(&self) -> &[Playlist] {
        &self.playlists
    }

    pub fn selected_id(&self) -> Option<PlaylistId> {
        self.selected_id
    }

    pub fn has_selected_playlist(&self) -> bool {
        self.selected_id.is_some()
    }

    pub fn can_shuffle_selected(&self) -> bool {
        self.selected_playlist()
            .is_some_and(|playlist| playlist.track_ids.len() > 1)
    }

    pub fn selected_playlist(&self) -> Option<&Playlist> {
        let id = self.selected_id?;
        self.playlists.iter().find(|playlist| playlist.id == id)
    }

    pub fn create(&mut self, name: &str) -> Result<PlaylistId, PlaylistError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(PlaylistError::EmptyName);
        }
        if self
            .playlists
            .iter()
            .any(|playlist| playlist.name.eq_ignore_ascii_case(name))
        {
            return Err(PlaylistError::DuplicateName);
        }

        self.next_id = self.next_id.saturating_add(1);
        let id = self.next_id;
        self.playlists.push(Playlist {
            id,
            name: name.to_owned(),
            track_ids: Vec::new(),
        });
        self.selected_id = Some(id);
        Ok(id)
    }

    pub fn select(&mut self, id: PlaylistId) -> bool {
        if self.playlists.iter().any(|playlist| playlist.id == id) {
            self.selected_id = Some(id);
            true
        } else {
            false
        }
    }

    pub fn add_tracks(&mut self, tracks: &[Track]) -> Result<usize, PlaylistError> {
        self.add_tracks_with_duplicates(tracks, false)
    }

    pub fn add_tracks_with_duplicates(
        &mut self,
        tracks: &[Track],
        allow_duplicates: bool,
    ) -> Result<usize, PlaylistError> {
        let Some(id) = self.selected_id else {
            return Err(PlaylistError::NoPlaylistSelected);
        };
        let playlist = self
            .playlists
            .iter_mut()
            .find(|playlist| playlist.id == id)
            .ok_or(PlaylistError::NoPlaylistSelected)?;

        let mut added = 0;
        for track in tracks {
            if allow_duplicates || !playlist.track_ids.contains(&track.id) {
                playlist.track_ids.push(track.id);
                added += 1;
            }
        }
        Ok(added)
    }

    pub fn shuffle_selected(&mut self) -> Result<(), PlaylistError> {
        let id = self.selected_id.ok_or(PlaylistError::NoPlaylistSelected)?;
        self.shuffle_playlist(id)
    }

    pub fn shuffle_playlist(&mut self, id: PlaylistId) -> Result<(), PlaylistError> {
        self.shuffle_playlist_with_rng(id, &mut fastrand::Rng::new())
    }

    #[cfg(test)]
    fn shuffle_selected_with_rng(&mut self, rng: &mut fastrand::Rng) -> Result<(), PlaylistError> {
        let id = self.selected_id.ok_or(PlaylistError::NoPlaylistSelected)?;
        self.shuffle_playlist_with_rng(id, rng)
    }

    fn shuffle_playlist_with_rng(
        &mut self,
        id: PlaylistId,
        rng: &mut fastrand::Rng,
    ) -> Result<(), PlaylistError> {
        let playlist = self
            .playlists
            .iter_mut()
            .find(|playlist| playlist.id == id)
            .ok_or(PlaylistError::NoPlaylistSelected)?;
        rng.shuffle(&mut playlist.track_ids);
        Ok(())
    }

    pub fn rename(&mut self, id: PlaylistId, name: &str) -> Result<(), PlaylistError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(PlaylistError::EmptyName);
        }
        if self
            .playlists
            .iter()
            .any(|playlist| playlist.id != id && playlist.name.eq_ignore_ascii_case(name))
        {
            return Err(PlaylistError::DuplicateName);
        }
        let playlist = self
            .playlists
            .iter_mut()
            .find(|playlist| playlist.id == id)
            .ok_or(PlaylistError::NoPlaylistSelected)?;
        playlist.name = name.to_owned();
        Ok(())
    }

    pub fn remove(&mut self, id: PlaylistId) -> bool {
        let Some(position) = self.playlists.iter().position(|playlist| playlist.id == id) else {
            return false;
        };
        self.playlists.remove(position);
        if self.selected_id == Some(id) {
            self.selected_id = None;
        }
        true
    }

    pub fn remove_selected_entry(&mut self, position: usize) -> bool {
        let Some(id) = self.selected_id else {
            return false;
        };
        self.remove_entry(id, position)
    }

    pub fn remove_entry(&mut self, id: PlaylistId, position: usize) -> bool {
        let Some(playlist) = self.playlists.iter_mut().find(|playlist| playlist.id == id) else {
            return false;
        };
        if position >= playlist.track_ids.len() {
            return false;
        }
        playlist.track_ids.remove(position);
        true
    }

    pub fn move_selected_entry(&mut self, from: usize, to: usize) -> bool {
        let Some(id) = self.selected_id else {
            return false;
        };
        self.move_entry(id, from, to)
    }

    pub fn move_entry(&mut self, id: PlaylistId, from: usize, to: usize) -> bool {
        let Some(playlist) = self.playlists.iter_mut().find(|playlist| playlist.id == id) else {
            return false;
        };
        if from >= playlist.track_ids.len() || to >= playlist.track_ids.len() {
            return false;
        }
        let entry = playlist.track_ids.remove(from);
        playlist.track_ids.insert(to, entry);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{
        export_m3u8, parse_m3u8, M3u8EntrySource, M3u8Error, Playlist, PlaylistCollection,
        PlaylistError,
    };
    use crate::model::{Track, TrackSource};
    use std::path::PathBuf;

    fn track(id: i32) -> Track {
        Track {
            id,
            path: PathBuf::from(format!("track-{id}.mp3")),
            title: format!("Track {id}"),
            format: "MP3".to_owned(),
            source: TrackSource::Local,
        }
    }

    #[test]
    fn m3u8_import_preserves_unicode_relative_paths_duplicates_and_network_urls() {
        let directory = PathBuf::from("音乐/playlists");
        let data = "\u{feff}#EXTM3U\r\n#EXTINF:-1,曲名\r\n../音楽/曲.flac\r\n../音楽/曲.flac\r\nhttps://example.test/live?token=signed\r\n";
        let entries = parse_m3u8(data.as_bytes(), &directory).unwrap();

        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].title.as_deref(), Some("曲名"));
        assert_eq!(entries[1].title, None);
        assert_eq!(
            entries[0].source,
            M3u8EntrySource::Local(directory.join("../音楽/曲.flac"))
        );
        assert_eq!(entries[1].source, entries[0].source);
        assert!(matches!(entries[2].source, M3u8EntrySource::Network(_)));
    }

    #[test]
    fn m3u8_import_is_bounded_and_rejects_unknown_url_schemes() {
        let directory = PathBuf::from("playlist");
        assert_eq!(
            parse_m3u8(b"file:unexpected", &directory),
            Err(M3u8Error::InvalidSource)
        );
        assert_eq!(
            parse_m3u8(&vec![b'x'; 2 * 1024 * 1024 + 1], &directory),
            Err(M3u8Error::TooLarge)
        );
        let too_many = "track.mp3\n".repeat(10_001);
        assert_eq!(
            parse_m3u8(too_many.as_bytes(), &directory),
            Err(M3u8Error::TooManyEntries)
        );
        let overlong_line = format!("{}\n", "x".repeat(8193));
        assert_eq!(
            parse_m3u8(overlong_line.as_bytes(), &directory),
            Err(M3u8Error::InvalidLine)
        );
        assert_eq!(
            parse_m3u8(b"#EXTM3U\n#EXTINF:-1,x\nlong\n", &directory),
            Ok(vec![super::M3u8Entry {
                source: M3u8EntrySource::Local(directory.join("long")),
                title: Some("x".into()),
            }])
        );
    }

    #[test]
    fn m3u8_export_keeps_duplicate_occurrences_and_writes_relative_native_paths() {
        #[cfg(windows)]
        let directory = PathBuf::from("C:/音乐");
        #[cfg(not(windows))]
        let directory = std::env::temp_dir().join("Rust VLC 音乐");
        let track = Track {
            id: 7,
            path: directory.join("disc 1/曲.flac"),
            title: "曲名".into(),
            format: "FLAC".into(),
            source: TrackSource::Local,
        };
        let playlist = Playlist {
            id: 2,
            name: "Favorites".into(),
            track_ids: vec![7, 7],
        };
        let output = export_m3u8(&playlist, &[track], &directory).unwrap();
        assert_eq!(
            output,
            "#EXTM3U\n#EXTINF:-1,曲名\n./disc 1/曲.flac\n#EXTINF:-1,曲名\n./disc 1/曲.flac\n"
        );
    }

    #[test]
    fn m3u8_relative_comment_and_scheme_like_filenames_round_trip_with_duplicates() {
        let directory = PathBuf::from("music");
        let hash_track = Track {
            id: 1,
            path: PathBuf::from("#song.mp3"),
            title: "Hash song".into(),
            format: "MP3".into(),
            source: TrackSource::Local,
        };
        let colon_track = Track {
            id: 2,
            path: PathBuf::from("disc:track.mp3"),
            title: "Colon track".into(),
            format: "MP3".into(),
            source: TrackSource::Local,
        };
        let playlist = Playlist {
            id: 1,
            name: "Special names".into(),
            track_ids: vec![1, 2, 1],
        };

        let output = export_m3u8(&playlist, &[hash_track, colon_track], &directory).unwrap();
        assert!(output.contains("./#song.mp3\n"));
        assert!(output.contains("./disc:track.mp3\n"));

        let imported = parse_m3u8(output.as_bytes(), &directory).unwrap();
        assert_eq!(imported.len(), 3);
        assert_eq!(
            imported[0].source,
            M3u8EntrySource::Local(directory.join("#song.mp3"))
        );
        assert_eq!(
            imported[1].source,
            M3u8EntrySource::Local(directory.join("disc:track.mp3"))
        );
        assert_eq!(imported[2], imported[0]);
    }

    #[cfg(unix)]
    #[test]
    fn m3u8_export_rejects_native_paths_that_cannot_round_trip_as_utf8() {
        use std::os::unix::ffi::OsStringExt;

        let path = PathBuf::from(std::ffi::OsString::from_vec(b"/music/\xff.mp3".to_vec()));
        let track = Track {
            id: 1,
            path,
            title: "Track".into(),
            format: "MP3".into(),
            source: TrackSource::Local,
        };
        let playlist = Playlist {
            id: 1,
            name: "Native".into(),
            track_ids: vec![1],
        };
        assert_eq!(
            export_m3u8(&playlist, &[track], std::path::Path::new("/music")),
            Err(M3u8Error::UnrepresentablePath)
        );
    }

    #[test]
    fn playlists_require_a_unique_nonempty_name() {
        let mut playlists = PlaylistCollection::default();
        assert_eq!(playlists.create("  "), Err(PlaylistError::EmptyName));
        assert_eq!(playlists.create("  Road Mix  "), Ok(1));
        assert_eq!(playlists.playlists()[0].name, "Road Mix");
        assert_eq!(
            playlists.create("road mix"),
            Err(PlaylistError::DuplicateName)
        );
    }

    #[test]
    fn adding_selected_tracks_keeps_order_and_does_not_duplicate() {
        let mut playlists = PlaylistCollection::default();
        playlists.create("Road Mix").unwrap();

        assert_eq!(playlists.add_tracks(&[track(3), track(1)]), Ok(2));
        assert_eq!(playlists.add_tracks(&[track(1), track(2)]), Ok(1));
        assert_eq!(playlists.playlists()[0].track_ids, [3, 1, 2]);
    }

    #[test]
    fn adding_duplicate_occurrences_is_an_explicit_choice() {
        let mut skipped = PlaylistCollection::default();
        skipped.create("Skip duplicates").unwrap();
        assert_eq!(
            skipped.add_tracks_with_duplicates(&[track(1), track(1)], false),
            Ok(1)
        );
        assert_eq!(skipped.playlists()[0].track_ids, [1]);

        let mut kept = PlaylistCollection::default();
        kept.create("Keep duplicates").unwrap();
        assert_eq!(
            kept.add_tracks_with_duplicates(&[track(1), track(1)], true),
            Ok(2)
        );
        assert_eq!(kept.playlists()[0].track_ids, [1, 1]);
    }

    #[test]
    fn tracks_cannot_be_added_without_a_selected_playlist() {
        let mut playlists = PlaylistCollection::default();
        assert_eq!(
            playlists.add_tracks(&[track(1)]),
            Err(PlaylistError::NoPlaylistSelected)
        );
    }

    #[test]
    fn shuffling_selected_playlist_changes_order_and_preserves_each_track_once() {
        let mut playlists = PlaylistCollection::default();
        playlists.create("Road Mix").unwrap();
        playlists
            .add_tracks(&[track(1), track(2), track(3), track(4), track(5)])
            .unwrap();
        let original = playlists.selected_playlist().unwrap().track_ids.clone();
        assert!(playlists.can_shuffle_selected());

        playlists
            .shuffle_selected_with_rng(&mut fastrand::Rng::with_seed(0x5eed))
            .unwrap();

        let shuffled = &playlists.selected_playlist().unwrap().track_ids;
        assert_ne!(shuffled, &original);
        let mut sorted_shuffled = shuffled.clone();
        sorted_shuffled.sort_unstable();
        assert_eq!(sorted_shuffled, original);
    }

    #[test]
    fn playlist_occurrence_edits_preserve_duplicates_and_order() {
        let mut playlists = PlaylistCollection::default();
        playlists.create("Duplicates").unwrap();
        playlists
            .add_tracks_with_duplicates(&[track(1), track(1), track(2)], true)
            .unwrap();

        assert!(playlists.move_selected_entry(0, 2));
        assert_eq!(playlists.playlists()[0].track_ids, [1, 2, 1]);
        assert!(playlists.remove_selected_entry(1));
        assert_eq!(playlists.playlists()[0].track_ids, [1, 1]);
        assert!(!playlists.remove_selected_entry(2));
        assert!(!playlists.move_selected_entry(0, 2));
    }

    #[test]
    fn shuffle_by_id_does_not_change_the_selected_playlist() {
        let mut playlists = PlaylistCollection::default();
        let first = playlists.create("First").unwrap();
        playlists
            .add_tracks(&[track(1), track(2), track(3), track(4)])
            .unwrap();
        let selected = playlists.create("Selected").unwrap();
        let original = playlists.playlists()[0].track_ids.clone();

        playlists.shuffle_playlist(first).unwrap();

        assert_eq!(playlists.selected_id(), Some(selected));
        let shuffled = &playlists.playlists()[0].track_ids;
        let mut sorted = shuffled.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, original);
    }

    #[test]
    fn editing_an_open_playlist_does_not_change_the_add_destination() {
        let mut playlists = PlaylistCollection::default();
        let opened = playlists.create("Opened").unwrap();
        playlists
            .add_tracks_with_duplicates(&[track(1), track(2), track(1)], true)
            .unwrap();
        let target = playlists.create("Destination").unwrap();
        assert_eq!(
            playlists.rename(opened, " Destination "),
            Err(PlaylistError::DuplicateName)
        );
        assert_eq!(
            playlists.rename(opened, "  "),
            Err(PlaylistError::EmptyName)
        );
        assert_eq!(playlists.rename(opened, " Renamed "), Ok(()));
        assert!(playlists.move_entry(opened, 2, 0));
        assert!(playlists.remove_entry(opened, 1));
        assert_eq!(playlists.playlists()[0].name, "Renamed");
        assert_eq!(playlists.playlists()[0].track_ids, [1, 2]);
        assert_eq!(playlists.selected_id(), Some(target));
        assert!(playlists.remove(opened));
        assert_eq!(playlists.selected_id(), Some(target));
        assert!(!playlists.remove(opened));
        assert!(playlists.remove(target));
        assert_eq!(playlists.selected_id(), None);
    }
}
