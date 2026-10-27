use crate::model::{MediaDetails, TrackId};
use crate::playback::{PlaybackSnapshot, PlaybackUiState};
#[cfg(windows)]
use raw_window_handle::RawWindowHandle;
use souvlaki::{
    MediaControlEvent, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig, SeekDirection,
};
use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const EVENT_CAPACITY: usize = 32;
const PROGRESS_UPDATE_INTERVAL: Duration = Duration::from_secs(1);
const DEFAULT_SEEK_MS: i64 = 10_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MediaControlIntent {
    Play,
    Pause,
    Toggle,
    Stop,
    Next,
    Previous,
    SeekRelative(i64),
    SeekTo(i64),
    SetVolume(i32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SystemPlaybackState {
    Stopped,
    Paused,
    Playing,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MetadataCache {
    track_id: Option<TrackId>,
    load_generation: u64,
    title: String,
    artist: String,
    album: String,
    cover_reference: Option<String>,
    duration_ms: Option<u64>,
}

pub struct MediaControlBridge {
    controls: souvlaki::MediaControls,
    events: Receiver<MediaControlIntent>,
    last_metadata: Option<MetadataCache>,
    last_state: Option<SystemPlaybackState>,
    last_progress_update: Option<Instant>,
    #[cfg(target_os = "linux")]
    last_volume_percent: Option<i32>,
}

impl MediaControlBridge {
    pub fn new(window: &slint::Window) -> Result<Self, String> {
        #[cfg(target_os = "linux")]
        ensure_session_bus_available()?;

        let dbus_name = format!("premiere_{}", std::process::id());
        let display_name = "Première";
        let hwnd = windows_hwnd(window)?;
        let mut controls = souvlaki::MediaControls::new(PlatformConfig {
            display_name,
            dbus_name: &dbus_name,
            hwnd,
        })
        .map_err(|error| format!("Could not initialize OS media controls: {error}"))?;
        let (sender, events) = mpsc::sync_channel(EVENT_CAPACITY);
        controls
            .attach(move |event| {
                if let Some(intent) = map_event(event) {
                    // OS callbacks must not block if the UI has not drained recent events.
                    let _ = sender.try_send(intent);
                }
            })
            .map_err(|error| format!("Could not attach OS media controls: {error}"))?;

        Ok(Self {
            controls,
            events,
            last_metadata: None,
            last_state: None,
            last_progress_update: None,
            #[cfg(target_os = "linux")]
            last_volume_percent: None,
        })
    }

    pub fn drain_events(&self) -> Vec<MediaControlIntent> {
        self.events.try_iter().collect()
    }

    pub fn update(
        &mut self,
        snapshot: &PlaybackSnapshot,
        details: Option<&MediaDetails>,
        artwork: Option<&Path>,
    ) -> Result<(), String> {
        let metadata = metadata_cache(snapshot, details, artwork);
        if self.last_metadata.as_ref() != Some(&metadata) {
            self.controls
                .set_metadata(MediaMetadata {
                    title: Some(&metadata.title),
                    artist: Some(&metadata.artist),
                    album: Some(&metadata.album),
                    cover_url: metadata.cover_reference.as_deref(),
                    duration: metadata.duration_ms.map(Duration::from_millis),
                })
                .map_err(|error| format!("Could not update OS media metadata: {error}"))?;
            self.last_metadata = Some(metadata.clone());
        }

        let system_state = match snapshot.state {
            PlaybackUiState::Playing => SystemPlaybackState::Playing,
            PlaybackUiState::Paused => SystemPlaybackState::Paused,
            _ => SystemPlaybackState::Stopped,
        };
        let now = Instant::now();
        let update_progress = self.last_state != Some(system_state)
            || self
                .last_progress_update
                .is_none_or(|last| now.duration_since(last) >= PROGRESS_UPDATE_INTERVAL);
        if update_progress {
            let progress = Some(MediaPosition(Duration::from_millis(
                snapshot.position_ms.max(0) as u64,
            )));
            let playback = match system_state {
                SystemPlaybackState::Playing => MediaPlayback::Playing { progress },
                SystemPlaybackState::Paused => MediaPlayback::Paused { progress },
                SystemPlaybackState::Stopped => MediaPlayback::Stopped,
            };
            self.controls
                .set_playback(playback)
                .map_err(|error| format!("Could not update OS media playback state: {error}"))?;
            self.last_state = Some(system_state);
            self.last_progress_update = Some(now);
        }

        #[cfg(target_os = "linux")]
        if self.last_volume_percent != Some(snapshot.volume_percent) {
            self.controls
                .set_volume(f64::from(snapshot.volume_percent.clamp(0, 100)) / 100.0)
                .map_err(|error| format!("Could not update OS media volume: {error}"))?;
            self.last_volume_percent = Some(snapshot.volume_percent);
        }

        Ok(())
    }
}

fn map_event(event: MediaControlEvent) -> Option<MediaControlIntent> {
    match event {
        MediaControlEvent::Play => Some(MediaControlIntent::Play),
        MediaControlEvent::Pause => Some(MediaControlIntent::Pause),
        MediaControlEvent::Toggle => Some(MediaControlIntent::Toggle),
        MediaControlEvent::Stop => Some(MediaControlIntent::Stop),
        MediaControlEvent::Next => Some(MediaControlIntent::Next),
        MediaControlEvent::Previous => Some(MediaControlIntent::Previous),
        MediaControlEvent::Seek(direction) => Some(MediaControlIntent::SeekRelative(
            seek_offset_ms(direction, DEFAULT_SEEK_MS as u128),
        )),
        MediaControlEvent::SeekBy(direction, duration) => {
            let millis = duration.as_millis();
            (millis > 0)
                .then(|| MediaControlIntent::SeekRelative(seek_offset_ms(direction, millis)))
        }
        MediaControlEvent::SetPosition(MediaPosition(position)) => Some(
            MediaControlIntent::SeekTo(position.as_millis().min(i64::MAX as u128) as i64),
        ),
        MediaControlEvent::SetVolume(volume) if volume.is_finite() => Some(
            MediaControlIntent::SetVolume((volume.clamp(0.0, 1.0) * 100.0).round() as i32),
        ),
        MediaControlEvent::SetVolume(_)
        | MediaControlEvent::OpenUri(_)
        | MediaControlEvent::Raise
        | MediaControlEvent::Quit => None,
    }
}

fn seek_offset_ms(direction: SeekDirection, millis: u128) -> i64 {
    let millis = millis.min(i64::MAX as u128) as i64;
    match direction {
        SeekDirection::Forward => millis,
        SeekDirection::Backward => -millis,
    }
}

fn metadata_cache(
    snapshot: &PlaybackSnapshot,
    details: Option<&MediaDetails>,
    artwork: Option<&Path>,
) -> MetadataCache {
    let artist = details
        .map(|details| {
            if details.artist.is_empty() {
                details.album_artist.as_str()
            } else {
                details.artist.as_str()
            }
        })
        .unwrap_or_default();
    let cover_path = artwork.or_else(|| details.and_then(|details| details.artwork.as_deref()));
    MetadataCache {
        track_id: snapshot.current_id,
        load_generation: snapshot.load_generation,
        title: snapshot.current_title.clone(),
        artist: artist.to_owned(),
        album: details.map_or_else(String::new, |details| details.album.clone()),
        cover_reference: cover_path.and_then(artwork_reference),
        duration_ms: (snapshot.duration_ms > 0).then_some(snapshot.duration_ms as u64),
    }
}

fn artwork_reference(path: &Path) -> Option<String> {
    if !path.is_file() {
        return None;
    }
    #[cfg(windows)]
    {
        windows_artwork_reference(path)
    }
    #[cfg(not(windows))]
    {
        reqwest::Url::from_file_path(path)
            .ok()
            .map(|url| url.to_string())
    }
}

#[cfg(windows)]
fn windows_artwork_reference(path: &Path) -> Option<String> {
    // shortcut: Souvlaki 0.8.3 strips `file://` and expects a native path; revisit when upstream accepts standard file URIs.
    let path = ordinary_windows_path(path)?;
    if !path.is_absolute() {
        return None;
    }
    Some(format!("file://{}", path.to_str()?))
}

#[cfg(all(windows, test))]
fn file_url(path: &Path) -> Option<String> {
    if !path.is_absolute() {
        return None;
    }
    let path = ordinary_windows_path(path)?;
    reqwest::Url::from_file_path(path)
        .ok()
        .map(|url| url.to_string())
}

#[cfg(windows)]
fn ordinary_windows_path(path: &Path) -> Option<std::path::PathBuf> {
    use std::path::PathBuf;

    let path_text = path.to_str()?;
    if let Some(unc_path) = path_text.strip_prefix("\\\\?\\UNC\\") {
        return Some(PathBuf::from(format!("\\\\{unc_path}")));
    }
    if let Some(drive_path) = path_text.strip_prefix("\\\\?\\") {
        return Some(PathBuf::from(drive_path));
    }
    Some(path.to_path_buf())
}

#[cfg(windows)]
fn windows_hwnd(window: &slint::Window) -> Result<Option<*mut std::ffi::c_void>, String> {
    use raw_window_handle::HasWindowHandle;

    let raw = window
        .window_handle()
        .window_handle()
        .map_err(|error| format!("The app window handle is not available yet: {error}"))?
        .as_raw();
    Ok(Some(windows_hwnd_from_raw(raw)?))
}

#[cfg(windows)]
fn windows_hwnd_from_raw(raw: RawWindowHandle) -> Result<*mut std::ffi::c_void, String> {
    match raw {
        RawWindowHandle::Win32(handle) => Ok(handle.hwnd.get() as *mut std::ffi::c_void),
        _ => Err("OS media controls require the app's Win32 window handle.".to_owned()),
    }
}

#[cfg(not(windows))]
fn windows_hwnd(_window: &slint::Window) -> Result<Option<*mut std::ffi::c_void>, String> {
    Ok(None)
}

#[cfg(target_os = "linux")]
fn ensure_session_bus_available() -> Result<(), String> {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixStream};

    let address = std::env::var("DBUS_SESSION_BUS_ADDRESS")
        .map_err(|_| "OS media controls require an active D-Bus session bus.".to_owned())?;
    let mut last_error = None;
    for address in address.split(';').filter(|part| part.starts_with("unix:")) {
        let connection = if let Some(path) = address
            .strip_prefix("unix:path=")
            .and_then(|part| part.split(',').next())
        {
            UnixStream::connect(decode_dbus_path(path)?)
        } else if let Some(name) = address
            .strip_prefix("unix:abstract=")
            .and_then(|part| part.split(',').next())
        {
            let name = decode_dbus_path(name)?;
            let address = SocketAddr::from_abstract_name(name.as_bytes())
                .map_err(|error| format!("The D-Bus session address is invalid: {error}"))?;
            UnixStream::connect_addr(&address)
        } else {
            // shortcut: preflight supports path and abstract sockets, add dir/tmpdir discovery if a supported target uses those D-Bus transports.
            last_error = Some("the D-Bus address uses an unsupported Unix transport".to_owned());
            continue;
        };
        match connection {
            Ok(_) => return Ok(()),
            Err(error) => last_error = Some(error.to_string()),
        }
    }
    Err(last_error.map_or_else(
        || "OS media controls require a Unix D-Bus session bus.".to_owned(),
        |error| format!("The D-Bus session socket is unavailable: {error}"),
    ))
}

#[cfg(target_os = "linux")]
fn decode_dbus_path(encoded: &str) -> Result<String, String> {
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let pair = bytes.get(index + 1..index + 3).ok_or_else(|| {
                "The D-Bus session bus address contains an invalid path.".to_owned()
            })?;
            let hex = std::str::from_utf8(pair)
                .ok()
                .and_then(|value| u8::from_str_radix(value, 16).ok())
                .ok_or_else(|| {
                    "The D-Bus session bus address contains an invalid path.".to_owned()
                })?;
            decoded.push(hex);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded)
        .map_err(|_| "The D-Bus session bus address contains an invalid path.".to_owned())
}

#[cfg(test)]
mod tests {
    use super::{map_event, metadata_cache, seek_offset_ms, MediaControlIntent};
    use crate::model::MediaDetails;
    use crate::playback::PlaybackSnapshot;
    use souvlaki::{MediaControlEvent, MediaPosition, SeekDirection};
    use std::time::Duration;

    #[test]
    fn os_events_become_owned_ui_intents() {
        assert_eq!(
            map_event(MediaControlEvent::Play),
            Some(MediaControlIntent::Play)
        );
        assert_eq!(
            map_event(MediaControlEvent::Pause),
            Some(MediaControlIntent::Pause)
        );
        assert_eq!(
            map_event(MediaControlEvent::SeekBy(
                SeekDirection::Backward,
                Duration::from_millis(2_500),
            )),
            Some(MediaControlIntent::SeekRelative(-2_500))
        );
        assert_eq!(
            map_event(MediaControlEvent::SetPosition(MediaPosition(
                Duration::from_millis(12_345),
            ))),
            Some(MediaControlIntent::SeekTo(12_345))
        );
        assert_eq!(
            map_event(MediaControlEvent::SetVolume(0.456)),
            Some(MediaControlIntent::SetVolume(46))
        );
        assert_eq!(
            map_event(MediaControlEvent::OpenUri("file:///tmp/a".to_owned())),
            None
        );
        assert_eq!(map_event(MediaControlEvent::SetVolume(f64::NAN)), None);
    }

    #[test]
    fn generic_seek_event_uses_a_bounded_ten_second_step() {
        assert_eq!(seek_offset_ms(SeekDirection::Forward, 10_000), 10_000);
        assert_eq!(seek_offset_ms(SeekDirection::Backward, 10_000), -10_000);
        assert_eq!(seek_offset_ms(SeekDirection::Forward, u128::MAX), i64::MAX);
    }

    #[test]
    fn metadata_cache_tracks_media_identity_and_album_artist_fallback() {
        let snapshot = PlaybackSnapshot {
            current_id: Some(7),
            load_generation: 3,
            current_title: "A song".to_owned(),
            duration_ms: 61_000,
            ..PlaybackSnapshot::default()
        };
        let details = MediaDetails {
            album_artist: "Artist".to_owned(),
            album: "Album".to_owned(),
            ..MediaDetails::default()
        };
        let metadata = metadata_cache(&snapshot, Some(&details), None);

        assert_eq!(metadata.track_id, Some(7));
        assert_eq!(metadata.load_generation, 3);
        assert_eq!(metadata.title, "A song");
        assert_eq!(metadata.artist, "Artist");
        assert_eq!(metadata.album, "Album");
        assert_eq!(metadata.duration_ms, Some(61_000));
    }

    #[cfg(windows)]
    #[test]
    fn windows_artwork_carrier_preserves_native_extended_drive_and_unc_paths() {
        use std::path::Path;

        let drive = Path::new("\\\\?\\C:\\Media\\poster été #1.png");
        assert_eq!(
            super::windows_artwork_reference(drive)
                .as_deref()
                .and_then(|reference| reference.strip_prefix("file://")),
            Some("C:\\Media\\poster été #1.png")
        );
        let unc = Path::new("\\\\?\\UNC\\server\\share\\poster art.png");
        assert_eq!(
            super::windows_artwork_reference(unc)
                .as_deref()
                .and_then(|reference| reference.strip_prefix("file://")),
            Some("\\\\server\\share\\poster art.png")
        );
        assert_eq!(
            super::file_url(drive).as_deref(),
            Some("file:///C:/Media/poster%20%C3%A9t%C3%A9%20%231.png")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn dbus_path_percent_escapes_are_decoded() {
        assert_eq!(
            super::decode_dbus_path("/run/user/1000/my%20bus").unwrap(),
            "/run/user/1000/my bus"
        );
        assert!(super::decode_dbus_path("/run/user/1000/%G0").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_handle_conversion_preserves_a_nonnull_hwnd() {
        use raw_window_handle::{RawWindowHandle, Win32WindowHandle};
        use std::num::NonZeroIsize;

        let raw = RawWindowHandle::Win32(Win32WindowHandle::new(
            NonZeroIsize::new(123).expect("test HWND is non-zero"),
        ));
        let hwnd = super::windows_hwnd_from_raw(raw).expect("Win32 HWND should be accepted");
        assert_eq!(hwnd as isize, 123);
    }
}
