use std::ffi::OsStr;
use std::path::{Path, PathBuf};

pub type TrackId = i32;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Track {
    pub id: TrackId,
    pub path: PathBuf,
    pub title: String,
    pub format: String,
}

impl Track {
    pub fn from_path(id: TrackId, path: PathBuf) -> Option<Self> {
        if !path.is_file() || !is_supported_audio_path(&path) {
            return None;
        }

        let title = readable_filename(&path);
        let format = path
            .extension()
            .and_then(OsStr::to_str)
            .map(str::to_ascii_uppercase)
            .unwrap_or_else(|| "AUDIO".to_owned());

        Some(Self {
            id,
            path,
            title,
            format,
        })
    }
}

pub fn is_supported_audio_path(path: &Path) -> bool {
    const AUDIO_EXTENSIONS: &[&str] = &[
        "aac", "aif", "aiff", "alac", "ape", "flac", "m4a", "m4b", "mid", "midi", "mp3", "mpc",
        "oga", "ogg", "opus", "wav", "wma", "wv",
    ];

    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            AUDIO_EXTENSIONS
                .iter()
                .any(|allowed| extension.eq_ignore_ascii_case(allowed))
        })
}

pub fn readable_filename(path: &Path) -> String {
    let raw = path
        .file_stem()
        .unwrap_or_else(|| path.file_name().unwrap_or_default())
        .to_string_lossy();

    let mut title = String::with_capacity(raw.len());
    let mut previous_space = true;
    for character in raw.chars() {
        let character = if matches!(character, '_' | '.' | '-') {
            ' '
        } else {
            character
        };
        if character.is_whitespace() {
            if !previous_space {
                title.push(' ');
            }
            previous_space = true;
        } else {
            title.push(character);
            previous_space = false;
        }
    }

    let title = title.trim();
    if title.is_empty() {
        "Unknown audio track".to_owned()
    } else {
        title.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::{is_supported_audio_path, readable_filename};
    use std::path::Path;

    #[test]
    fn filename_fallback_is_readable_and_keeps_unicode() {
        assert_eq!(
            readable_filename(Path::new("03_After-the Rain.flac")),
            "03 After the Rain"
        );
        assert_eq!(readable_filename(Path::new("デモ_音楽.mp3")), "デモ 音楽");
    }

    #[test]
    fn audio_filter_is_case_insensitive_and_does_not_accept_video() {
        assert!(is_supported_audio_path(Path::new("song.FLAC")));
        assert!(is_supported_audio_path(Path::new("song.m4a")));
        assert!(!is_supported_audio_path(Path::new("movie.mkv")));
    }
}
