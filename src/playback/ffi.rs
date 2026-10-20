//! Minimal dynamic libVLC 3 boundary for local audio playback.
//!
//! The signatures and ownership rules below follow VideoLAN's libVLC 3 C API:
//! `libvlc_media_player_set_media` retains the media, the temporary media
//! reference can then be released, and the media player must be released before
//! its instance. No callbacks are registered. `VlcEngine` is constructed and
//! used only by the serialized playback worker, and the `Library` stays loaded
//! until after the player and instance have been released.

use crate::model::Track;
use crate::platform::{self, RuntimeCandidate};
use libloading::Library;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;

type LibVlcGetVersion = unsafe extern "C" fn() -> *const c_char;
type LibVlcNew = unsafe extern "C" fn(c_int, *const *const c_char) -> *mut c_void;
type LibVlcRelease = unsafe extern "C" fn(*mut c_void);
type LibVlcErrmsg = unsafe extern "C" fn() -> *const c_char;
type LibVlcMediaNewPath = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void;
type LibVlcMediaRelease = unsafe extern "C" fn(*mut c_void);
type LibVlcMediaPlayerNew = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type LibVlcMediaPlayerRelease = unsafe extern "C" fn(*mut c_void);
type LibVlcMediaPlayerSetMedia = unsafe extern "C" fn(*mut c_void, *mut c_void);
type LibVlcMediaPlayerPlay = unsafe extern "C" fn(*mut c_void) -> c_int;
type LibVlcMediaPlayerStop = unsafe extern "C" fn(*mut c_void);
type LibVlcMediaPlayerSetPause = unsafe extern "C" fn(*mut c_void, c_int);
type LibVlcMediaPlayerGetState = unsafe extern "C" fn(*mut c_void) -> c_int;
type LibVlcMediaPlayerGetTime = unsafe extern "C" fn(*mut c_void) -> i64;
type LibVlcMediaPlayerGetLength = unsafe extern "C" fn(*mut c_void) -> i64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VlcError {
    RuntimeNotFound,
    PluginsNotFound {
        library_path: PathBuf,
        plugin_path: PathBuf,
    },
    LoadFailed(String),
    MissingSymbol {
        library_path: PathBuf,
        symbol: String,
    },
    UnsupportedVersion {
        library_path: PathBuf,
        version: String,
    },
    InitializationFailed(String),
    PathNotUtf8(PathBuf),
    InvalidPath(PathBuf),
    MediaCreationFailed(String),
    PlaybackFailed(String),
}

impl fmt::Display for VlcError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RuntimeNotFound => write!(
                formatter,
                "libVLC 3 was not found. Install VLC 3 with its audio plugins, or set VLC_DIR to an absolute VLC installation directory."
            ),
            Self::PluginsNotFound {
                library_path,
                plugin_path,
            } => write!(
                formatter,
                "VLC was found at {}, but its plugins folder is missing at {}. Reinstall VLC with the standard plugins, or set VLC_DIR to the complete installation directory.",
                library_path.display(),
                plugin_path.display()
            ),
            Self::LoadFailed(attempts) => write!(
                formatter,
                "A libVLC runtime was found but could not be loaded: {attempts}. Install a matching VLC 3 runtime, including its dependent libraries."
            ),
            Self::MissingSymbol {
                library_path,
                symbol,
            } => write!(
                formatter,
                "{} does not export required libVLC 3 symbol {symbol}. Use a complete libVLC 3 installation.",
                library_path.display()
            ),
            Self::UnsupportedVersion {
                library_path,
                version,
            } => write!(
                formatter,
                "{} reports libVLC version {version}; this audio slice requires the libVLC 3 ABI.",
                library_path.display()
            ),
            Self::InitializationFailed(message) => write!(
                formatter,
                "libVLC 3 could not initialize. Check that the VLC plugins folder is readable. {message}"
            ),
            Self::PathNotUtf8(path) => write!(
                formatter,
                "This file path cannot be represented as UTF-8 for libVLC 3: {}",
                path.display()
            ),
            Self::InvalidPath(path) => write!(
                formatter,
                "The selected audio file is no longer available: {}",
                path.display()
            ),
            Self::MediaCreationFailed(message) => write!(
                formatter,
                "libVLC could not open the selected audio file. Check that the file still exists and that the VLC audio codec plugins are installed. {message}"
            ),
            Self::PlaybackFailed(message) => write!(
                formatter,
                "libVLC could not start audio playback. Check the file's codec and the VLC audio output plugins. {message}"
            ),
        }
    }
}

impl std::error::Error for VlcError {}

#[derive(Clone, Copy)]
struct Api {
    get_version: LibVlcGetVersion,
    new: LibVlcNew,
    release: LibVlcRelease,
    errmsg: LibVlcErrmsg,
    media_new_path: LibVlcMediaNewPath,
    media_release: LibVlcMediaRelease,
    player_new: LibVlcMediaPlayerNew,
    player_release: LibVlcMediaPlayerRelease,
    player_set_media: LibVlcMediaPlayerSetMedia,
    player_play: LibVlcMediaPlayerPlay,
    player_stop: LibVlcMediaPlayerStop,
    player_set_pause: LibVlcMediaPlayerSetPause,
    player_get_state: LibVlcMediaPlayerGetState,
    player_get_time: LibVlcMediaPlayerGetTime,
    player_get_length: LibVlcMediaPlayerGetLength,
}

pub struct Runtime {
    // Kept alive until the engine's Drop has released the player and instance.
    _library: Library,
    api: Api,
    pub version: String,
}

impl Runtime {
    pub fn load() -> Result<Self, VlcError> {
        Self::load_candidates(platform::runtime_candidates())
    }

    fn load_candidates(candidates: Vec<RuntimeCandidate>) -> Result<Self, VlcError> {
        let mut missing_plugins = None;
        let mut load_failures = Vec::new();

        for candidate in candidates {
            if !candidate.library_path.is_absolute() || !candidate.library_path.is_file() {
                continue;
            }
            if !candidate.plugin_path.is_dir() {
                missing_plugins = Some((candidate.library_path, candidate.plugin_path));
                continue;
            }

            match unsafe { Self::load_candidate(candidate.clone()) } {
                Ok(runtime) => return Ok(runtime),
                Err(error) => load_failures.push(error.to_string()),
            }
        }

        if !load_failures.is_empty() {
            return Err(VlcError::LoadFailed(load_failures.join("; ")));
        }
        if let Some((library_path, plugin_path)) = missing_plugins {
            return Err(VlcError::PluginsNotFound {
                library_path,
                plugin_path,
            });
        }
        Err(VlcError::RuntimeNotFound)
    }

    /// Load the documented libVLC 3 functions from a single candidate.
    ///
    /// # Safety
    /// `candidate.library_path` must identify a trusted libVLC installation,
    /// and the function signatures must match its exported C ABI. Candidates
    /// are built only from VLC_DIR, the app bundle, or standard install paths.
    unsafe fn load_candidate(candidate: RuntimeCandidate) -> Result<Self, VlcError> {
        #[cfg(windows)]
        let load_result = unsafe {
            use libloading::os::windows::{
                Library as WindowsLibrary, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
            };
            WindowsLibrary::load_with_flags(
                &candidate.library_path,
                LOAD_LIBRARY_SEARCH_DEFAULT_DIRS | LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
            )
            .map(Library::from)
        };

        #[cfg(not(windows))]
        let load_result = unsafe { Library::new(&candidate.library_path) };

        let library = load_result.map_err(|error| {
            VlcError::LoadFailed(format!(
                "{} ({error}). Ensure the VLC library and its dependent files are available in the same installation directory.",
                candidate.library_path.display()
            ))
        })?;
        let path = candidate.library_path.clone();
        let api = Api {
            get_version: unsafe { symbol(&library, b"libvlc_get_version\0", &path)? },
            new: unsafe { symbol(&library, b"libvlc_new\0", &path)? },
            release: unsafe { symbol(&library, b"libvlc_release\0", &path)? },
            errmsg: unsafe { symbol(&library, b"libvlc_errmsg\0", &path)? },
            media_new_path: unsafe { symbol(&library, b"libvlc_media_new_path\0", &path)? },
            media_release: unsafe { symbol(&library, b"libvlc_media_release\0", &path)? },
            player_new: unsafe { symbol(&library, b"libvlc_media_player_new\0", &path)? },
            player_release: unsafe { symbol(&library, b"libvlc_media_player_release\0", &path)? },
            player_set_media: unsafe {
                symbol(&library, b"libvlc_media_player_set_media\0", &path)?
            },
            player_play: unsafe { symbol(&library, b"libvlc_media_player_play\0", &path)? },
            player_stop: unsafe { symbol(&library, b"libvlc_media_player_stop\0", &path)? },
            player_set_pause: unsafe {
                symbol(&library, b"libvlc_media_player_set_pause\0", &path)?
            },
            player_get_state: unsafe {
                symbol(&library, b"libvlc_media_player_get_state\0", &path)?
            },
            player_get_time: unsafe { symbol(&library, b"libvlc_media_player_get_time\0", &path)? },
            player_get_length: unsafe {
                symbol(&library, b"libvlc_media_player_get_length\0", &path)?
            },
        };

        let version_pointer = unsafe { (api.get_version)() };
        let version = if version_pointer.is_null() {
            "unknown".to_owned()
        } else {
            unsafe { CStr::from_ptr(version_pointer) }
                .to_string_lossy()
                .into_owned()
        };
        if !is_libvlc_3(&version) {
            return Err(VlcError::UnsupportedVersion {
                library_path: candidate.library_path,
                version,
            });
        }

        Ok(Self {
            _library: library,
            api,
            version,
        })
    }

    fn last_error(&self) -> String {
        let pointer = unsafe { (self.api.errmsg)() };
        if pointer.is_null() {
            "VLC did not provide an error message.".to_owned()
        } else {
            unsafe { CStr::from_ptr(pointer) }
                .to_string_lossy()
                .into_owned()
        }
    }
}

unsafe fn symbol<T: Copy>(
    library: &Library,
    name: &'static [u8],
    path: &Path,
) -> Result<T, VlcError> {
    // The copied function pointer remains valid because Runtime owns `library`
    // for the whole lifetime of every VlcEngine using this API table.
    unsafe { library.get::<T>(name) }
        .map(|symbol| *symbol)
        .map_err(|_| VlcError::MissingSymbol {
            library_path: path.to_path_buf(),
            symbol: String::from_utf8_lossy(&name[..name.len() - 1]).into_owned(),
        })
}

fn is_libvlc_3(version: &str) -> bool {
    version.trim().split('.').next() == Some("3")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlayerState {
    Opening,
    Buffering,
    Playing,
    Paused,
    Stopped,
    Ended,
    Error,
    Other,
}

impl PlayerState {
    fn from_libvlc(value: c_int) -> Self {
        match value {
            1 => Self::Opening,
            2 => Self::Buffering,
            3 => Self::Playing,
            4 => Self::Paused,
            5 => Self::Stopped,
            6 => Self::Ended,
            7 => Self::Error,
            _ => Self::Other,
        }
    }
}

pub struct VlcEngine {
    runtime: Runtime,
    instance: NonNull<c_void>,
    player: NonNull<c_void>,
}

impl VlcEngine {
    pub fn new() -> Result<Self, VlcError> {
        let runtime = Runtime::load()?;
        let no_video_arg = CString::new("--no-video").expect("static argument has no NUL byte");
        let arguments = [no_video_arg.as_ptr()];
        let instance = NonNull::new(unsafe {
            (runtime.api.new)(arguments.len() as c_int, arguments.as_ptr())
        })
        .ok_or_else(|| VlcError::InitializationFailed(runtime.last_error()))?;

        let player = NonNull::new(unsafe { (runtime.api.player_new)(instance.as_ptr()) });
        let Some(player) = player else {
            let message = runtime.last_error();
            unsafe { (runtime.api.release)(instance.as_ptr()) };
            return Err(VlcError::InitializationFailed(message));
        };

        Ok(Self {
            runtime,
            instance,
            player,
        })
    }

    pub fn version(&self) -> &str {
        &self.runtime.version
    }

    pub fn play(&mut self, track: &Track) -> Result<(), VlcError> {
        self.stop_and_clear();

        if !track.path.is_file() {
            return Err(VlcError::InvalidPath(track.path.clone()));
        }

        let path_string = track
            .path
            .to_str()
            .ok_or_else(|| VlcError::PathNotUtf8(track.path.clone()))?;
        #[cfg(windows)]
        let path_string = path_string.replace('/', "\\");
        let native_path =
            CString::new(path_string).map_err(|_| VlcError::PathNotUtf8(track.path.clone()))?;

        let media = NonNull::new(unsafe {
            (self.runtime.api.media_new_path)(self.instance.as_ptr(), native_path.as_ptr())
        })
        .ok_or_else(|| VlcError::MediaCreationFailed(self.runtime.last_error()))?;

        unsafe {
            (self.runtime.api.player_set_media)(self.player.as_ptr(), media.as_ptr());
            (self.runtime.api.media_release)(media.as_ptr());
        }

        if unsafe { (self.runtime.api.player_play)(self.player.as_ptr()) } != 0 {
            let message = self.runtime.last_error();
            self.stop_and_clear();
            return Err(VlcError::PlaybackFailed(message));
        }
        Ok(())
    }

    fn stop_and_clear(&self) {
        unsafe {
            (self.runtime.api.player_stop)(self.player.as_ptr());
            (self.runtime.api.player_set_media)(self.player.as_ptr(), std::ptr::null_mut());
        }
    }

    pub fn toggle_pause(&self) {
        let state = self.state();
        let should_pause = state != PlayerState::Paused;
        unsafe {
            (self.runtime.api.player_set_pause)(self.player.as_ptr(), i32::from(should_pause));
        }
    }

    pub fn state(&self) -> PlayerState {
        PlayerState::from_libvlc(unsafe {
            (self.runtime.api.player_get_state)(self.player.as_ptr())
        })
    }

    pub fn time_ms(&self) -> i64 {
        unsafe { (self.runtime.api.player_get_time)(self.player.as_ptr()) }
    }

    pub fn length_ms(&self) -> i64 {
        unsafe { (self.runtime.api.player_get_length)(self.player.as_ptr()) }
    }

    pub fn error_message(&self) -> String {
        self.runtime.last_error()
    }
}

impl Drop for VlcEngine {
    fn drop(&mut self) {
        unsafe {
            (self.runtime.api.player_stop)(self.player.as_ptr());
            (self.runtime.api.player_release)(self.player.as_ptr());
            (self.runtime.api.release)(self.instance.as_ptr());
        }
        // `runtime` and its dynamic library are dropped after this method.
    }
}

#[cfg(test)]
mod tests {
    use super::{is_libvlc_3, Runtime, VlcError};
    use crate::model::Track;
    use crate::platform::RuntimeCandidate;
    use crate::playback::{PlaybackCommand, PlaybackUiState, PlaybackWorker};
    use std::f64::consts::TAU;
    use std::fs;
    use std::path::PathBuf;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn missing_runtime_has_actionable_install_guidance() {
        let error = match Runtime::load_candidates(Vec::<RuntimeCandidate>::new()) {
            Ok(_) => panic!("an empty candidate list cannot load libVLC"),
            Err(error) => error,
        };
        let message = error.to_string();
        assert!(matches!(error, VlcError::RuntimeNotFound));
        assert!(message.contains("Install VLC 3"));
        assert!(message.contains("VLC_DIR"));
    }

    #[test]
    fn only_the_libvlc_3_abi_is_accepted() {
        assert!(is_libvlc_3("3.0.22 Vetinari"));
        assert!(!is_libvlc_3("4.0.0"));
        assert!(!is_libvlc_3("unknown"));
    }

    #[test]
    #[ignore = "requires an installed libVLC 3 runtime"]
    fn missing_next_stops_previous_audio_and_keeps_the_queue_snapshot_truthful() {
        let path = temporary_wave_path();
        write_test_wave(&path);
        let _temporary_file = TemporaryFile(path.clone());
        let track =
            Track::from_path(1, path.clone()).expect("generated WAV should be a supported track");
        let missing_path = path.with_extension("missing.wav");
        assert!(!missing_path.exists());
        let missing_track = Track {
            id: 2,
            path: missing_path.clone(),
            title: "Missing next track".to_owned(),
            format: "WAV".to_owned(),
        };
        let worker = PlaybackWorker::spawn().expect("playback worker should start");
        worker
            .submit(PlaybackCommand::PlayQueue {
                tracks: vec![track, missing_track],
                start_index: 0,
            })
            .expect("worker should accept the test queue");

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = worker.snapshot();
            match snapshot.state {
                PlaybackUiState::Playing => break,
                PlaybackUiState::Error | PlaybackUiState::Unavailable => {
                    panic!("libVLC did not start the test WAV: {}", snapshot.message)
                }
                _ if Instant::now() < deadline => thread::sleep(Duration::from_millis(40)),
                state => panic!("libVLC did not reach Playing; final state: {state:?}"),
            }
        }

        worker
            .submit(PlaybackCommand::Next)
            .expect("worker should accept Next");
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let snapshot = worker.snapshot();
            if snapshot.state == PlaybackUiState::Error {
                break;
            }
            assert_ne!(
                snapshot.state,
                PlaybackUiState::Unavailable,
                "libVLC became unavailable: {}",
                snapshot.message
            );
            assert!(
                Instant::now() < deadline,
                "missing next file did not produce a playback error: {snapshot:?}"
            );
            thread::sleep(Duration::from_millis(40));
        }

        // Allow the worker's regular VLC-state poll to run after the failed start.
        thread::sleep(Duration::from_millis(400));
        let snapshot = worker.snapshot();
        assert_eq!(snapshot.current_id, Some(2));
        assert_eq!(snapshot.current_title, "Missing next track");
        assert_eq!(snapshot.queue_index, 2);
        assert_eq!(snapshot.queue_len, 2);
        assert_eq!(snapshot.state, PlaybackUiState::Error);
        assert_eq!(snapshot.position_ms, 0);
        assert_eq!(snapshot.duration_ms, 0);
        assert!(snapshot
            .message
            .contains(&missing_path.display().to_string()));
    }

    struct TemporaryFile(PathBuf);

    impl Drop for TemporaryFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn temporary_wave_path() -> PathBuf {
        std::env::temp_dir().join(format!("rust-vlc-audio-smoke-{}.wav", std::process::id()))
    }

    fn write_test_wave(path: &std::path::Path) {
        const SAMPLE_RATE: u32 = 44_100;
        const SECONDS: u32 = 5;
        let frames = SAMPLE_RATE * SECONDS;
        let data_size = frames * 2;
        let mut bytes = Vec::with_capacity(44 + data_size as usize);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_size).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
        bytes.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
        bytes.extend_from_slice(&2_u16.to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_size.to_le_bytes());

        for frame in 0..frames {
            let phase = TAU * 440.0 * f64::from(frame) / f64::from(SAMPLE_RATE);
            let sample = (phase.sin() * 0.1 * f64::from(i16::MAX)) as i16;
            bytes.extend_from_slice(&sample.to_le_bytes());
        }

        fs::write(path, bytes).expect("test WAV should be writable in the system temp directory");
    }
}
