use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::{fmt, str::FromStr};

pub type TrackId = i32;

const MAX_NETWORK_URL_BYTES: usize = 4096;

/// A validated network media address. Credentials embedded in URL userinfo are rejected.
#[derive(Clone, Eq, PartialEq)]
pub struct NetworkUrl(reqwest::Url);

impl fmt::Debug for NetworkUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("NetworkUrl")
            .field(&self.display_label())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkUrlError {
    Empty,
    TooLong,
    Invalid,
    UnsupportedScheme,
    MissingHost,
    EmbeddedCredentials,
}

impl fmt::Display for NetworkUrlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "Enter a network URL.",
            Self::TooLong => "The network URL is longer than 4096 bytes.",
            Self::Invalid => "The network URL is invalid.",
            Self::UnsupportedScheme => "Use an HTTP, HTTPS, RTSP, or RTMP URL.",
            Self::MissingHost => "The network URL must include a host.",
            Self::EmbeddedCredentials => "Remove credentials from the network URL.",
        })
    }
}

impl std::error::Error for NetworkUrlError {}

impl NetworkUrl {
    pub fn parse(input: &str) -> Result<Self, NetworkUrlError> {
        let input = input.trim();
        if input.is_empty() {
            return Err(NetworkUrlError::Empty);
        }
        if input.len() > MAX_NETWORK_URL_BYTES {
            return Err(NetworkUrlError::TooLong);
        }
        if input.chars().any(char::is_control) {
            return Err(NetworkUrlError::Invalid);
        }
        let url = reqwest::Url::from_str(input).map_err(|_| NetworkUrlError::Invalid)?;
        if url.as_str().len() > MAX_NETWORK_URL_BYTES {
            return Err(NetworkUrlError::TooLong);
        }
        if !matches!(url.scheme(), "http" | "https" | "rtsp" | "rtmp") {
            return Err(NetworkUrlError::UnsupportedScheme);
        }
        if url.host_str().is_none() {
            return Err(NetworkUrlError::MissingHost);
        }
        let has_userinfo_marker = input
            .split_once("://")
            .and_then(|(_, remainder)| remainder.split(['/', '?', '#']).next())
            .is_some_and(|authority| authority.contains('@'));
        if has_userinfo_marker || !url.username().is_empty() || url.password().is_some() {
            return Err(NetworkUrlError::EmbeddedCredentials);
        }
        Ok(Self(url))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// This value is safe for a history label. The actual URL remains unchanged for playback.
    pub fn display_label(&self) -> String {
        let mut url = self.0.clone();
        url.set_query(None);
        url.set_fragment(None);
        url.to_string()
    }

    /// The actual stream address is preserved privately so signed URLs remain playable.
    pub fn for_persistence(&self) -> Self {
        self.clone()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TrackSource {
    Local,
    Network(NetworkUrl),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MediaDetails {
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub genre: String,
    pub disc_number: Option<u32>,
    pub track_number: Option<u32>,
    pub artwork: Option<PathBuf>,
    pub artwork_override: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Track {
    pub id: TrackId,
    /// Native local path. Empty for network tracks; never used to carry a URL.
    pub path: PathBuf,
    pub title: String,
    pub format: String,
    pub source: TrackSource,
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
            source: TrackSource::Local,
        })
    }

    pub fn from_network_url(id: TrackId, input: &str) -> Result<Self, NetworkUrlError> {
        Self::from_network_url_with_format(id, input, "MOVIE")
    }

    pub fn from_network_audio_url(id: TrackId, input: &str) -> Result<Self, NetworkUrlError> {
        Self::from_network_url_with_format(id, input, "STREAM")
    }

    fn from_network_url_with_format(
        id: TrackId,
        input: &str,
        format: &str,
    ) -> Result<Self, NetworkUrlError> {
        let source = NetworkUrl::parse(input)?;
        let title = source
            .0
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .filter(|segment| !segment.is_empty())
            .map(percent_encoding_decode)
            .filter(|title| !title.trim().is_empty())
            .or_else(|| source.0.host_str().map(str::to_owned))
            .unwrap_or_else(|| "Network stream".to_owned());
        Ok(Self {
            id,
            path: PathBuf::new(),
            title,
            format: format.to_owned(),
            source: TrackSource::Network(source),
        })
    }
}

fn percent_encoding_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let high = (bytes[index + 1] as char).to_digit(16);
            let low = (bytes[index + 2] as char).to_digit(16);
            if let (Some(high), Some(low)) = (high, low) {
                decoded.push((high * 16 + low) as u8);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(decoded).unwrap_or_else(|_| value.to_owned())
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
    use super::{
        is_supported_audio_path, readable_filename, NetworkUrl, NetworkUrlError, Track,
        TrackSource, MAX_NETWORK_URL_BYTES,
    };
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

    #[test]
    fn network_tracks_keep_urls_out_of_native_paths() {
        let track = Track::from_network_url(7, "https://example.test/live/été").unwrap();
        assert!(track.path.as_os_str().is_empty());
        assert_eq!(track.title, "été");
        assert!(matches!(track.source, TrackSource::Network(_)));
        match track.source {
            TrackSource::Network(url) => {
                assert_eq!(url.as_str(), "https://example.test/live/%C3%A9t%C3%A9")
            }
            TrackSource::Local => panic!("network constructor returned local source"),
        }
    }

    #[test]
    fn network_url_validation_is_explicit_and_allows_user_selected_lan_hosts() {
        assert!(NetworkUrl::parse("rtsp://192.168.1.20/camera").is_ok());
        assert!(matches!(
            NetworkUrl::parse("file:///etc/passwd"),
            Err(NetworkUrlError::UnsupportedScheme)
        ));
        assert!(matches!(
            NetworkUrl::parse("https://user:secret@example.test/live"),
            Err(NetworkUrlError::EmbeddedCredentials)
        ));
        assert!(matches!(
            NetworkUrl::parse("https://@example.test/live"),
            Err(NetworkUrlError::EmbeddedCredentials)
        ));
        assert!(matches!(
            NetworkUrl::parse("https://example.test/\nsecret"),
            Err(NetworkUrlError::Invalid)
        ));
        assert_eq!(
            NetworkUrl::parse(&format!("https://x.test/{}", "x".repeat(4096))),
            Err(NetworkUrlError::TooLong)
        );

        let expands_past_limit = format!("https://example.test/live?q={}", "é".repeat(1500));
        assert!(expands_past_limit.len() < MAX_NETWORK_URL_BYTES);
        assert_eq!(
            NetworkUrl::parse(&expands_past_limit),
            Err(NetworkUrlError::TooLong)
        );

        let near_limit = format!("https://example.test/live?q={}", "é".repeat(675));
        let parsed = NetworkUrl::parse(&near_limit).expect("valid encoded URL should fit the cap");
        assert!(parsed.as_str().len() <= MAX_NETWORK_URL_BYTES);
    }

    #[test]
    fn network_url_storage_preserves_signed_queries_and_display_redacts_them() {
        let url = NetworkUrl::parse(
            "https://example.test/live?token=private&quality=high&signature=signed#private",
        )
        .unwrap();
        assert_eq!(
            url.for_persistence().as_str(),
            "https://example.test/live?token=private&quality=high&signature=signed#private"
        );
        assert_eq!(url.display_label(), "https://example.test/live");
        assert!(!format!("{url:?}").contains("private"));
        assert!(!format!("{url:?}").contains("signed"));
    }
}
