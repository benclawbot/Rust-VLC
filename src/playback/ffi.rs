//! Minimal dynamic libVLC 3 boundary for local audio and video playback.
//!
//! The signatures and ownership rules below follow VideoLAN's libVLC 3 C API:
//! `libvlc_media_player_set_media` retains the media, the temporary media
//! reference can then be released, and the media player must be released before
//! its instance. Video callbacks use only preallocated aligned buffers and an
//! owned frame mailbox; they do not call libVLC or Slint. `VlcEngine` is
//! constructed and used only by the serialized playback worker, and the
//! `Library` stays loaded until after the player and instance have been
//! released.

use super::frame::{CallbackFrame, FrameLayout, FrameMailbox, MAX_IN_FLIGHT_FRAMES};
use super::is_movie;
use super::{
    seek_target_ms, valid_cast_address, valid_equalizer_settings, valid_media_delay,
    valid_playback_rate, valid_volume_percent, EqualizerSettings, PlaybackTrack,
    EQUALIZER_BAND_COUNT,
};
use crate::model::{Track, TrackSource};
use crate::platform::{self, RuntimeCandidate};
use libloading::Library;
use reqwest::Url;
use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

type LibVlcGetVersion = unsafe extern "C" fn() -> *const c_char;
type LibVlcNew = unsafe extern "C" fn(c_int, *const *const c_char) -> *mut c_void;
type LibVlcRelease = unsafe extern "C" fn(*mut c_void);
type LibVlcErrmsg = unsafe extern "C" fn() -> *const c_char;
type LibVlcMediaNewPath = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void;
type LibVlcMediaNewLocation = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void;
type LibVlcMediaRelease = unsafe extern "C" fn(*mut c_void);
type LibVlcMediaAddOption = unsafe extern "C" fn(*mut c_void, *const c_char);
type LibVlcMediaPlayerNew = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type LibVlcMediaPlayerRelease = unsafe extern "C" fn(*mut c_void);
type LibVlcMediaPlayerSetMedia = unsafe extern "C" fn(*mut c_void, *mut c_void);
type LibVlcMediaPlayerPlay = unsafe extern "C" fn(*mut c_void) -> c_int;
type LibVlcMediaPlayerStop = unsafe extern "C" fn(*mut c_void);
type LibVlcMediaPlayerSetPause = unsafe extern "C" fn(*mut c_void, c_int);
type LibVlcMediaPlayerGetState = unsafe extern "C" fn(*mut c_void) -> c_int;
type LibVlcMediaPlayerGetTime = unsafe extern "C" fn(*mut c_void) -> i64;
type LibVlcMediaPlayerGetLength = unsafe extern "C" fn(*mut c_void) -> i64;
type LibVlcMediaPlayerSetTime = unsafe extern "C" fn(*mut c_void, i64);
type LibVlcMediaPlayerIsSeekable = unsafe extern "C" fn(*mut c_void) -> c_int;
type LibVlcMediaPlayerSetRate = unsafe extern "C" fn(*mut c_void, f32) -> c_int;
type LibVlcEqualizerGetPresetCount = unsafe extern "C" fn() -> c_uint;
type LibVlcEqualizerGetPresetName = unsafe extern "C" fn(c_uint) -> *const c_char;
type LibVlcEqualizerGetBandCount = unsafe extern "C" fn() -> c_uint;
type LibVlcEqualizerGetBandFrequency = unsafe extern "C" fn(c_uint) -> f32;
type LibVlcEqualizerNew = unsafe extern "C" fn() -> *mut c_void;
type LibVlcEqualizerNewFromPreset = unsafe extern "C" fn(c_uint) -> *mut c_void;
type LibVlcEqualizerRelease = unsafe extern "C" fn(*mut c_void);
type LibVlcEqualizerSetPreamp = unsafe extern "C" fn(*mut c_void, f32) -> c_int;
type LibVlcEqualizerGetPreamp = unsafe extern "C" fn(*mut c_void) -> f32;
type LibVlcEqualizerSetAmpAtIndex = unsafe extern "C" fn(*mut c_void, f32, c_uint) -> c_int;
type LibVlcEqualizerGetAmpAtIndex = unsafe extern "C" fn(*mut c_void, c_uint) -> f32;
type LibVlcMediaPlayerSetEqualizer = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;
type LibVlcAudioSetVolume = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
type LibVlcAudioGetTrackCount = unsafe extern "C" fn(*mut c_void) -> c_int;
type LibVlcAudioGetTrackDescription =
    unsafe extern "C" fn(*mut c_void) -> *mut LibVlcTrackDescription;
type LibVlcAudioGetTrack = unsafe extern "C" fn(*mut c_void) -> c_int;
type LibVlcAudioSetTrack = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
type LibVlcAudioGetDelay = unsafe extern "C" fn(*mut c_void) -> i64;
type LibVlcAudioSetDelay = unsafe extern "C" fn(*mut c_void, i64) -> c_int;
type LibVlcVideoGetSpuCount = unsafe extern "C" fn(*mut c_void) -> c_int;
type LibVlcVideoGetSpuDescription =
    unsafe extern "C" fn(*mut c_void) -> *mut LibVlcTrackDescription;
type LibVlcVideoGetSpu = unsafe extern "C" fn(*mut c_void) -> c_int;
type LibVlcVideoSetSpu = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
type LibVlcVideoGetSpuDelay = unsafe extern "C" fn(*mut c_void) -> i64;
type LibVlcVideoSetSpuDelay = unsafe extern "C" fn(*mut c_void, i64) -> c_int;
type LibVlcMediaPlayerAddSlave =
    unsafe extern "C" fn(*mut c_void, c_int, *const c_char, bool) -> c_int;
type LibVlcTrackDescriptionListRelease = unsafe extern "C" fn(*mut LibVlcTrackDescription);
type LibVlcMediaPlayerGetChapter = unsafe extern "C" fn(*mut c_void) -> c_int;
type LibVlcMediaPlayerSetChapter = unsafe extern "C" fn(*mut c_void, c_int);
type LibVlcMediaPlayerGetChapterCount = unsafe extern "C" fn(*mut c_void) -> c_int;
type LibVlcMediaPlayerGetFullChapterDescriptions =
    unsafe extern "C" fn(*mut c_void, c_int, *mut *mut *mut LibVlcChapterDescription) -> c_int;
type LibVlcChapterDescriptionsRelease =
    unsafe extern "C" fn(*mut *mut LibVlcChapterDescription, c_uint);
type LibVlcVideoLock = unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> *mut c_void;
type LibVlcVideoUnlock = unsafe extern "C" fn(*mut c_void, *mut c_void, *const *mut c_void);
type LibVlcVideoDisplay = unsafe extern "C" fn(*mut c_void, *mut c_void);
type LibVlcVideoFormat = unsafe extern "C" fn(
    *mut *mut c_void,
    *mut c_char,
    *mut c_uint,
    *mut c_uint,
    *mut c_uint,
    *mut c_uint,
) -> c_uint;
type LibVlcVideoCleanup = unsafe extern "C" fn(*mut c_void);
type LibVlcVideoSetCallbacks = unsafe extern "C" fn(
    *mut c_void,
    Option<LibVlcVideoLock>,
    Option<LibVlcVideoUnlock>,
    Option<LibVlcVideoDisplay>,
    *mut c_void,
);
type LibVlcVideoSetFormatCallbacks =
    unsafe extern "C" fn(*mut c_void, Option<LibVlcVideoFormat>, Option<LibVlcVideoCleanup>);
type LibVlcMediaPlayerEventManager = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type LibVlcEventCallback = unsafe extern "C" fn(*const LibVlcEvent, *mut c_void);
type LibVlcEventAttach =
    unsafe extern "C" fn(*mut c_void, c_int, Option<LibVlcEventCallback>, *mut c_void) -> c_int;
type LibVlcEventDetach =
    unsafe extern "C" fn(*mut c_void, c_int, Option<LibVlcEventCallback>, *mut c_void);

const LIBVLC_MEDIA_PLAYER_ENCOUNTERED_ERROR: c_int = 0x10A;
const CAST_DEMUX_FILTER_OPTION: &str = ":demux-filter=cc_demux";

#[repr(C)]
struct LibVlcEvent {
    event_type: c_int,
}

#[repr(C)]
struct LibVlcTrackDescription {
    id: c_int,
    name: *mut c_char,
    next: *mut LibVlcTrackDescription,
}

#[repr(C)]
struct LibVlcChapterDescription {
    time_offset: i64,
    duration: i64,
    name: *mut c_char,
}

const MAX_MEDIA_DESCRIPTIONS: usize = 256;
const SUBTITLE_SLAVE_TYPE: c_int = 0;

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
    #[cfg(windows)]
    UnsupportedPathNamespace(PathBuf),
    MediaCreationFailed(String),
    PlaybackFailed(String),
    ControlFailed {
        action: &'static str,
        message: String,
    },
    InvalidControl(&'static str),
}

impl fmt::Display for VlcError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RuntimeNotFound => write!(
                formatter,
                "libVLC 3 was not found. Install VLC 3 with its media plugins, or set VLC_DIR to an absolute VLC installation directory."
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
                "{} reports libVLC version {version}; this media playback slice requires the libVLC 3 ABI.",
                library_path.display()
            ),
            Self::InitializationFailed(message) => write!(
                formatter,
                "libVLC 3 could not initialize. Check that the VLC plugins folder is readable. {message}"
            ),
            Self::PathNotUtf8(path) => write!(
                formatter,
                "This local media path cannot be represented as UTF-8 for libVLC 3: {}",
                path.display()
            ),
            Self::InvalidPath(path) => write!(
                formatter,
                "The selected local media file is no longer available: {}",
                path.display()
            ),
            #[cfg(windows)]
            Self::UnsupportedPathNamespace(path) => write!(
                formatter,
                "This Windows media path uses a namespace that libVLC does not support: {}",
                path.display()
            ),
            Self::MediaCreationFailed(message) => write!(
                formatter,
                "libVLC could not open the selected media. Check that it is available and that the VLC codec plugins are installed. {message}"
            ),
            Self::PlaybackFailed(message) => write!(
                formatter,
                "libVLC could not start media playback. Check the file's codec and the VLC output plugins. {message}"
            ),
            Self::ControlFailed { action, message } => {
                write!(formatter, "libVLC could not {action}: {message}")
            }
            Self::InvalidControl(control) => write!(formatter, "Invalid {control} value."),
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
    media_new_location: LibVlcMediaNewLocation,
    media_release: LibVlcMediaRelease,
    media_add_option: LibVlcMediaAddOption,
    player_new: LibVlcMediaPlayerNew,
    player_release: LibVlcMediaPlayerRelease,
    player_set_media: LibVlcMediaPlayerSetMedia,
    player_play: LibVlcMediaPlayerPlay,
    player_stop: LibVlcMediaPlayerStop,
    player_set_pause: LibVlcMediaPlayerSetPause,
    player_get_state: LibVlcMediaPlayerGetState,
    player_get_time: LibVlcMediaPlayerGetTime,
    player_get_length: LibVlcMediaPlayerGetLength,
    player_set_time: LibVlcMediaPlayerSetTime,
    player_is_seekable: LibVlcMediaPlayerIsSeekable,
    player_set_rate: LibVlcMediaPlayerSetRate,
    equalizer_get_preset_count: LibVlcEqualizerGetPresetCount,
    equalizer_get_preset_name: LibVlcEqualizerGetPresetName,
    equalizer_get_band_count: LibVlcEqualizerGetBandCount,
    equalizer_get_band_frequency: LibVlcEqualizerGetBandFrequency,
    equalizer_new: LibVlcEqualizerNew,
    equalizer_new_from_preset: LibVlcEqualizerNewFromPreset,
    equalizer_release: LibVlcEqualizerRelease,
    equalizer_set_preamp: LibVlcEqualizerSetPreamp,
    equalizer_get_preamp: LibVlcEqualizerGetPreamp,
    equalizer_set_amp_at_index: LibVlcEqualizerSetAmpAtIndex,
    equalizer_get_amp_at_index: LibVlcEqualizerGetAmpAtIndex,
    player_set_equalizer: LibVlcMediaPlayerSetEqualizer,
    audio_set_volume: LibVlcAudioSetVolume,
    audio_get_track_count: LibVlcAudioGetTrackCount,
    audio_get_track_description: LibVlcAudioGetTrackDescription,
    audio_get_track: LibVlcAudioGetTrack,
    audio_set_track: LibVlcAudioSetTrack,
    audio_get_delay: LibVlcAudioGetDelay,
    audio_set_delay: LibVlcAudioSetDelay,
    video_get_spu_count: LibVlcVideoGetSpuCount,
    video_get_spu_description: LibVlcVideoGetSpuDescription,
    video_get_spu: LibVlcVideoGetSpu,
    video_set_spu: LibVlcVideoSetSpu,
    video_get_spu_delay: LibVlcVideoGetSpuDelay,
    video_set_spu_delay: LibVlcVideoSetSpuDelay,
    media_player_add_slave: LibVlcMediaPlayerAddSlave,
    track_description_list_release: LibVlcTrackDescriptionListRelease,
    player_get_chapter: LibVlcMediaPlayerGetChapter,
    player_set_chapter: LibVlcMediaPlayerSetChapter,
    player_get_chapter_count: LibVlcMediaPlayerGetChapterCount,
    player_get_full_chapter_descriptions: LibVlcMediaPlayerGetFullChapterDescriptions,
    chapter_descriptions_release: LibVlcChapterDescriptionsRelease,
    video_set_callbacks: LibVlcVideoSetCallbacks,
    video_set_format_callbacks: LibVlcVideoSetFormatCallbacks,
    media_player_event_manager: LibVlcMediaPlayerEventManager,
    event_attach: LibVlcEventAttach,
    event_detach: LibVlcEventDetach,
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
        #[cfg(target_os = "macos")]
        let candidates = match platform::first_complete_runtime_candidate(&candidates) {
            // VLC_PLUGIN_PATH is process-wide, so macOS must keep the runtime selected at startup.
            Some(candidate) => vec![candidate],
            None => candidates,
        };
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
            media_new_location: unsafe { symbol(&library, b"libvlc_media_new_location\0", &path)? },
            media_release: unsafe { symbol(&library, b"libvlc_media_release\0", &path)? },
            media_add_option: unsafe { symbol(&library, b"libvlc_media_add_option\0", &path)? },
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
            player_set_time: unsafe { symbol(&library, b"libvlc_media_player_set_time\0", &path)? },
            player_is_seekable: unsafe {
                symbol(&library, b"libvlc_media_player_is_seekable\0", &path)?
            },
            player_set_rate: unsafe { symbol(&library, b"libvlc_media_player_set_rate\0", &path)? },
            equalizer_get_preset_count: unsafe {
                symbol(
                    &library,
                    b"libvlc_audio_equalizer_get_preset_count\0",
                    &path,
                )?
            },
            equalizer_get_preset_name: unsafe {
                symbol(&library, b"libvlc_audio_equalizer_get_preset_name\0", &path)?
            },
            equalizer_get_band_count: unsafe {
                symbol(&library, b"libvlc_audio_equalizer_get_band_count\0", &path)?
            },
            equalizer_get_band_frequency: unsafe {
                symbol(
                    &library,
                    b"libvlc_audio_equalizer_get_band_frequency\0",
                    &path,
                )?
            },
            equalizer_new: unsafe { symbol(&library, b"libvlc_audio_equalizer_new\0", &path)? },
            equalizer_new_from_preset: unsafe {
                symbol(&library, b"libvlc_audio_equalizer_new_from_preset\0", &path)?
            },
            equalizer_release: unsafe {
                symbol(&library, b"libvlc_audio_equalizer_release\0", &path)?
            },
            equalizer_set_preamp: unsafe {
                symbol(&library, b"libvlc_audio_equalizer_set_preamp\0", &path)?
            },
            equalizer_get_preamp: unsafe {
                symbol(&library, b"libvlc_audio_equalizer_get_preamp\0", &path)?
            },
            equalizer_set_amp_at_index: unsafe {
                symbol(
                    &library,
                    b"libvlc_audio_equalizer_set_amp_at_index\0",
                    &path,
                )?
            },
            equalizer_get_amp_at_index: unsafe {
                symbol(
                    &library,
                    b"libvlc_audio_equalizer_get_amp_at_index\0",
                    &path,
                )?
            },
            player_set_equalizer: unsafe {
                symbol(&library, b"libvlc_media_player_set_equalizer\0", &path)?
            },
            audio_set_volume: unsafe { symbol(&library, b"libvlc_audio_set_volume\0", &path)? },
            audio_get_track_count: unsafe {
                symbol(&library, b"libvlc_audio_get_track_count\0", &path)?
            },
            audio_get_track_description: unsafe {
                symbol(&library, b"libvlc_audio_get_track_description\0", &path)?
            },
            audio_get_track: unsafe { symbol(&library, b"libvlc_audio_get_track\0", &path)? },
            audio_set_track: unsafe { symbol(&library, b"libvlc_audio_set_track\0", &path)? },
            audio_get_delay: unsafe { symbol(&library, b"libvlc_audio_get_delay\0", &path)? },
            audio_set_delay: unsafe { symbol(&library, b"libvlc_audio_set_delay\0", &path)? },
            video_get_spu_count: unsafe {
                symbol(&library, b"libvlc_video_get_spu_count\0", &path)?
            },
            video_get_spu_description: unsafe {
                symbol(&library, b"libvlc_video_get_spu_description\0", &path)?
            },
            video_get_spu: unsafe { symbol(&library, b"libvlc_video_get_spu\0", &path)? },
            video_set_spu: unsafe { symbol(&library, b"libvlc_video_set_spu\0", &path)? },
            video_get_spu_delay: unsafe {
                symbol(&library, b"libvlc_video_get_spu_delay\0", &path)?
            },
            video_set_spu_delay: unsafe {
                symbol(&library, b"libvlc_video_set_spu_delay\0", &path)?
            },
            media_player_add_slave: unsafe {
                symbol(&library, b"libvlc_media_player_add_slave\0", &path)?
            },
            track_description_list_release: unsafe {
                symbol(&library, b"libvlc_track_description_list_release\0", &path)?
            },
            player_get_chapter: unsafe {
                symbol(&library, b"libvlc_media_player_get_chapter\0", &path)?
            },
            player_set_chapter: unsafe {
                symbol(&library, b"libvlc_media_player_set_chapter\0", &path)?
            },
            player_get_chapter_count: unsafe {
                symbol(&library, b"libvlc_media_player_get_chapter_count\0", &path)?
            },
            player_get_full_chapter_descriptions: unsafe {
                symbol(
                    &library,
                    b"libvlc_media_player_get_full_chapter_descriptions\0",
                    &path,
                )?
            },
            chapter_descriptions_release: unsafe {
                symbol(&library, b"libvlc_chapter_descriptions_release\0", &path)?
            },
            video_set_callbacks: unsafe {
                symbol(&library, b"libvlc_video_set_callbacks\0", &path)?
            },
            video_set_format_callbacks: unsafe {
                symbol(&library, b"libvlc_video_set_format_callbacks\0", &path)?
            },
            media_player_event_manager: unsafe {
                symbol(&library, b"libvlc_media_player_event_manager\0", &path)?
            },
            event_attach: unsafe { symbol(&library, b"libvlc_event_attach\0", &path)? },
            event_detach: unsafe { symbol(&library, b"libvlc_event_detach\0", &path)? },
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

fn copy_track_descriptions(
    reported_count: c_int,
    head: *mut LibVlcTrackDescription,
    release: LibVlcTrackDescriptionListRelease,
) -> Vec<PlaybackTrack> {
    let Some(head) = NonNull::new(head) else {
        return Vec::new();
    };
    let _guard = TrackDescriptionsGuard { head, release };
    let count = usize::try_from(reported_count)
        .unwrap_or_default()
        .min(MAX_MEDIA_DESCRIPTIONS);
    let mut tracks = Vec::with_capacity(count);
    let mut current = head.as_ptr();
    while !current.is_null() && tracks.len() < count {
        let description = unsafe { &*current };
        let name = if description.name.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(description.name) }
                .to_string_lossy()
                .trim()
                .to_owned()
        };
        let id = description.id;
        tracks.push(PlaybackTrack {
            id,
            name: if name.is_empty() {
                format!("Track {id}")
            } else {
                name
            },
        });
        current = description.next;
    }
    tracks
}

struct TrackDescriptionsGuard {
    head: NonNull<LibVlcTrackDescription>,
    release: LibVlcTrackDescriptionListRelease,
}

impl Drop for TrackDescriptionsGuard {
    fn drop(&mut self) {
        unsafe { (self.release)(self.head.as_ptr()) };
    }
}

struct ChapterDescriptionsGuard {
    descriptions: *mut *mut LibVlcChapterDescription,
    count: c_uint,
    release: LibVlcChapterDescriptionsRelease,
}

impl Drop for ChapterDescriptionsGuard {
    fn drop(&mut self) {
        unsafe { (self.release)(self.descriptions, self.count) };
    }
}

fn subtitle_file_uri(path: &Path) -> Result<CString, VlcError> {
    if !path.is_file() {
        return Err(VlcError::InvalidPath(path.to_path_buf()));
    }
    let absolute_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        path.canonicalize()
            .map_err(|_| VlcError::InvalidPath(path.to_path_buf()))?
    };
    let uri = Url::from_file_path(&absolute_path)
        .map_err(|_| VlcError::InvalidPath(path.to_path_buf()))?;
    CString::new(uri.as_str()).map_err(|_| VlcError::InvalidPath(path.to_path_buf()))
}

fn cast_media_options(address: &str) -> Result<[CString; 2], VlcError> {
    if !valid_cast_address(address) {
        return Err(VlcError::InvalidControl("cast target"));
    }
    let demux_filter = CString::new(CAST_DEMUX_FILTER_OPTION)
        .expect("static Chromecast media option has no NUL byte");
    let output = CString::new(format!(":sout=#chromecast{{ip={address}}}"))
        .expect("cast target validation rejects NUL bytes");
    Ok([demux_filter, output])
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct UnsupportedWindowsNamespace;

// libVLC 3 misparses verbatim paths as file URLs with a `?` host.
#[cfg(any(windows, test))]
fn windows_vlc_path(path: &str) -> Result<String, UnsupportedWindowsNamespace> {
    let path = path.replace('/', "\\");
    if let Some(verbatim_path) = path.strip_prefix(r"\\?\") {
        if let Some(unc_path) = verbatim_path
            .get(..4)
            .filter(|prefix| prefix.eq_ignore_ascii_case("UNC\\"))
            .map(|_| &verbatim_path[4..])
        {
            let mut components = unc_path.split('\\');
            if components.next().is_some_and(|server| !server.is_empty())
                && components.next().is_some_and(|share| !share.is_empty())
            {
                return Ok(format!(r"\\{unc_path}"));
            }
            return Err(UnsupportedWindowsNamespace);
        }

        let bytes = verbatim_path.as_bytes();
        if bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'\\'
        {
            return Ok(verbatim_path.to_owned());
        }
        return Err(UnsupportedWindowsNamespace);
    }

    if path.starts_with(r"\\.\") {
        return Err(UnsupportedWindowsNamespace);
    }

    Ok(path)
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

#[derive(Clone, Debug)]
pub(super) struct MediaControls {
    pub audio_tracks: Vec<PlaybackTrack>,
    pub subtitle_tracks: Vec<PlaybackTrack>,
    pub chapters: Vec<PlaybackTrack>,
    pub audio_track: c_int,
    pub subtitle_track: c_int,
    pub chapter: c_int,
    pub audio_delay_us: i64,
    pub subtitle_delay_us: i64,
}

impl Default for MediaControls {
    fn default() -> Self {
        Self {
            audio_tracks: Vec::new(),
            subtitle_tracks: Vec::new(),
            chapters: Vec::new(),
            audio_track: -1,
            subtitle_track: -1,
            chapter: -1,
            audio_delay_us: 0,
            subtitle_delay_us: 0,
        }
    }
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

struct VideoCallbackContext {
    frames: Arc<FrameMailbox>,
    state: Mutex<VideoCallbackState>,
    buffer_returned: Condvar,
    encountered_error: AtomicBool,
}

struct VideoCallbackState {
    layout: Option<FrameLayout>,
    buffers: Vec<CallbackFrame>,
    in_use: Vec<bool>,
    picture_ids: Vec<Option<usize>>,
    available: Vec<usize>,
    pending_frames: Vec<PendingVideoFrame>,
    next_picture_id: Option<usize>,
    reconfiguring: bool,
    shutting_down: bool,
}

struct PendingVideoFrame {
    picture_id: usize,
    frame: super::frame::VideoFrame,
}

impl VideoCallbackContext {
    fn lock_state(&self) -> MutexGuard<'_, VideoCallbackState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn reset_after_stop(&self) {
        let mut state = self.lock_state();
        while state.in_use.iter().any(|in_use| *in_use) {
            state = self
                .buffer_returned
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        state.pending_frames.clear();
        drop(state);
        self.frames.reset();
        self.encountered_error.store(false, Ordering::Release);
    }

    fn take_encountered_error(&self) -> bool {
        self.encountered_error.swap(false, Ordering::AcqRel)
    }
}

fn latch_player_error_event(event_type: c_int, encountered_error: &AtomicBool) {
    if event_type == LIBVLC_MEDIA_PLAYER_ENCOUNTERED_ERROR {
        encountered_error.store(true, Ordering::Release);
    }
}

unsafe extern "C" fn playback_event_callback(event: *const LibVlcEvent, opaque: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if event.is_null() || opaque.is_null() {
            return;
        }
        let context = unsafe { &*opaque.cast::<VideoCallbackContext>() };
        let event_type = unsafe { (*event).event_type };
        latch_player_error_event(event_type, &context.encountered_error);
    }));
}

unsafe fn callback_context(opaque: *mut c_void) -> Option<Arc<VideoCallbackContext>> {
    let pointer = opaque.cast::<VideoCallbackContext>();
    if pointer.is_null() {
        return None;
    }
    // The engine and the libVLC opaque reference keep this Arc allocation alive
    // until player release has stopped new callbacks.
    unsafe { Arc::increment_strong_count(pointer) };
    Some(unsafe { Arc::from_raw(pointer) })
}

unsafe extern "C" fn video_format(
    opaque: *mut *mut c_void,
    chroma: *mut c_char,
    width: *mut c_uint,
    height: *mut c_uint,
    pitches: *mut c_uint,
    lines: *mut c_uint,
) -> c_uint {
    catch_unwind(AssertUnwindSafe(|| {
        if opaque.is_null()
            || chroma.is_null()
            || width.is_null()
            || height.is_null()
            || pitches.is_null()
            || lines.is_null()
        {
            return 0;
        }
        let user_opaque = unsafe { *opaque };
        let Some(context) = (unsafe { callback_context(user_opaque) }) else {
            return 0;
        };
        let width_value = unsafe { *width };
        let height_value = unsafe { *height };
        let Some(row_bytes) = (width_value as usize).checked_mul(4) else {
            return 0;
        };
        let Some(pitch) = row_bytes.checked_add(63).map(|pitch| pitch & !63) else {
            return 0;
        };
        let Ok(pitch) = c_uint::try_from(pitch) else {
            return 0;
        };
        let Some(layout) = FrameLayout::new(width_value, height_value, pitch as usize) else {
            return 0;
        };

        let mut state = context.lock_state();
        state.reconfiguring = true;
        state.shutting_down = false;
        context.buffer_returned.notify_all();
        while state.in_use.iter().any(|in_use| *in_use) {
            state = context
                .buffer_returned
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        state.layout = None;
        state.buffers.clear();
        state.in_use.clear();
        state.picture_ids.clear();
        state.available.clear();
        state.pending_frames.clear();

        let buffers = (|| {
            let mut buffers = Vec::new();
            buffers.try_reserve_exact(MAX_IN_FLIGHT_FRAMES).ok()?;
            for _ in 0..MAX_IN_FLIGHT_FRAMES {
                buffers.push(
                    context
                        .frames
                        .allocate(layout, context.frames.generation())?,
                );
            }
            let mut in_use = Vec::new();
            in_use.try_reserve_exact(MAX_IN_FLIGHT_FRAMES).ok()?;
            in_use.resize(MAX_IN_FLIGHT_FRAMES, false);
            let mut picture_ids = Vec::new();
            picture_ids.try_reserve_exact(MAX_IN_FLIGHT_FRAMES).ok()?;
            picture_ids.resize(MAX_IN_FLIGHT_FRAMES, None);
            let mut available = Vec::new();
            available.try_reserve_exact(MAX_IN_FLIGHT_FRAMES).ok()?;
            available.extend((0..MAX_IN_FLIGHT_FRAMES).rev());
            let mut pending_frames = Vec::new();
            pending_frames
                .try_reserve_exact(MAX_IN_FLIGHT_FRAMES)
                .ok()?;
            Some((buffers, in_use, picture_ids, available, pending_frames))
        })();
        let Some((buffers, in_use, picture_ids, available, pending_frames)) = buffers else {
            state.reconfiguring = false;
            state.shutting_down = true;
            drop(state);
            context.buffer_returned.notify_all();
            return 0;
        };

        state.layout = Some(layout);
        state.buffers = buffers;
        state.in_use = in_use;
        state.picture_ids = picture_ids;
        state.available = available;
        state.pending_frames = pending_frames;
        state.reconfiguring = false;
        state.shutting_down = false;
        unsafe {
            std::ptr::copy_nonoverlapping(b"RGBA".as_ptr(), chroma.cast::<u8>(), 4);
            *pitches = pitch;
            *lines = height_value;
        }
        drop(state);
        context.buffer_returned.notify_all();
        MAX_IN_FLIGHT_FRAMES as c_uint
    }))
    .unwrap_or(0)
}

unsafe extern "C" fn video_cleanup(opaque: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(context) = unsafe { callback_context(opaque) } {
            let mut state = context.lock_state();
            state.reconfiguring = false;
            state.shutting_down = true;
            context.buffer_returned.notify_all();
            while state.in_use.iter().any(|in_use| *in_use) {
                state = context
                    .buffer_returned
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            state.layout = None;
            state.buffers.clear();
            state.in_use.clear();
            state.picture_ids.clear();
            state.available.clear();
            state.pending_frames.clear();
            drop(state);
            context.buffer_returned.notify_all();
        }
    }));
}

unsafe extern "C" fn video_lock(opaque: *mut c_void, planes: *mut *mut c_void) -> *mut c_void {
    catch_unwind(AssertUnwindSafe(|| {
        if planes.is_null() {
            return std::ptr::null_mut();
        }
        let Some(context) = (unsafe { callback_context(opaque) }) else {
            return std::ptr::null_mut();
        };
        let mut state = context.lock_state();
        loop {
            if state.shutting_down {
                return std::ptr::null_mut();
            }
            if state.reconfiguring {
                state = context
                    .buffer_returned
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                continue;
            }
            if state.layout.is_none() {
                return std::ptr::null_mut();
            }
            if let Some(&slot) = state.available.last() {
                let Some(picture_id) = state.next_picture_id else {
                    return std::ptr::null_mut();
                };
                state.next_picture_id = picture_id.checked_add(1);
                state.available.pop();
                state.in_use[slot] = true;
                state.picture_ids[slot] = Some(picture_id);
                let frame = &mut state.buffers[slot];
                frame.set_generation(context.frames.generation());
                let plane = frame.plane();
                unsafe { *planes = plane.cast() };
                // The picture is an integer cookie only. It is never dereferenced;
                // monotonic IDs keep late display callbacks distinct from reused planes.
                return picture_id as *mut c_void;
            }
            state = context
                .buffer_returned
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }))
    .unwrap_or(std::ptr::null_mut())
}

unsafe extern "C" fn video_unlock(
    opaque: *mut c_void,
    picture: *mut c_void,
    _planes: *const *mut c_void,
) {
    if picture.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(context) = unsafe { callback_context(opaque) } {
            let mut state = context.lock_state();
            let picture_id = picture as usize;
            let slot = state
                .picture_ids
                .iter()
                .position(|candidate| *candidate == Some(picture_id));
            if let Some(slot) = slot.filter(|slot| state.in_use[*slot]) {
                let frame = state.buffers[slot].copy_video_frame();
                state.in_use[slot] = false;
                state.picture_ids[slot] = None;
                state.available.push(slot);
                if state.pending_frames.len() < MAX_IN_FLIGHT_FRAMES {
                    if let Some(frame) = frame {
                        state
                            .pending_frames
                            .push(PendingVideoFrame { picture_id, frame });
                    }
                }
                drop(state);
                context.buffer_returned.notify_all();
            }
        }
    }));
}

unsafe extern "C" fn video_display(opaque: *mut c_void, picture: *mut c_void) {
    if picture.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(context) = unsafe { callback_context(opaque) } {
            let picture_id = picture as usize;
            let frame = {
                let mut state = context.lock_state();
                state
                    .pending_frames
                    .iter()
                    .position(|pending| pending.picture_id == picture_id)
                    .map(|index| state.pending_frames.swap_remove(index).frame)
            };
            if let Some(frame) = frame {
                context.frames.publish(frame);
            }
        }
    }));
}

pub struct VlcEngine {
    _video_context: Arc<VideoCallbackContext>,
    video_opaque: *const VideoCallbackContext,
    event_manager: NonNull<c_void>,
    runtime: Runtime,
    instance: NonNull<c_void>,
    player: NonNull<c_void>,
}

impl VlcEngine {
    pub fn new(frames: Arc<FrameMailbox>) -> Result<Self, VlcError> {
        let runtime = Runtime::load()?;
        let quiet = CString::new("--quiet").expect("static VLC option has no NUL byte");
        let arguments = [quiet.as_ptr()];
        let instance = NonNull::new(unsafe { (runtime.api.new)(1, arguments.as_ptr()) })
            .ok_or_else(|| VlcError::InitializationFailed(runtime.last_error()))?;

        let player = NonNull::new(unsafe { (runtime.api.player_new)(instance.as_ptr()) });
        let Some(player) = player else {
            let message = runtime.last_error();
            unsafe { (runtime.api.release)(instance.as_ptr()) };
            return Err(VlcError::InitializationFailed(message));
        };

        let video_context = Arc::new(VideoCallbackContext {
            frames,
            state: Mutex::new(VideoCallbackState {
                layout: None,
                buffers: Vec::new(),
                in_use: Vec::new(),
                picture_ids: Vec::new(),
                available: Vec::new(),
                pending_frames: Vec::new(),
                next_picture_id: Some(1),
                reconfiguring: false,
                shutting_down: false,
            }),
            buffer_returned: Condvar::new(),
            encountered_error: AtomicBool::new(false),
        });
        let video_opaque = Arc::into_raw(Arc::clone(&video_context));
        unsafe {
            (runtime.api.video_set_callbacks)(
                player.as_ptr(),
                Some(video_lock),
                Some(video_unlock),
                Some(video_display),
                video_opaque.cast_mut().cast(),
            );
            (runtime.api.video_set_format_callbacks)(
                player.as_ptr(),
                Some(video_format),
                Some(video_cleanup),
            );
        }

        let event_manager =
            NonNull::new(unsafe { (runtime.api.media_player_event_manager)(player.as_ptr()) });
        let Some(event_manager) = event_manager else {
            unsafe {
                (runtime.api.player_release)(player.as_ptr());
                (runtime.api.release)(instance.as_ptr());
                video_context.reset_after_stop();
                drop(Arc::from_raw(video_opaque));
            }
            return Err(VlcError::InitializationFailed(
                "libVLC could not monitor playback errors.".to_owned(),
            ));
        };
        let event_attach_result = unsafe {
            (runtime.api.event_attach)(
                event_manager.as_ptr(),
                LIBVLC_MEDIA_PLAYER_ENCOUNTERED_ERROR,
                Some(playback_event_callback),
                video_opaque.cast_mut().cast(),
            )
        };
        if event_attach_result != 0 {
            unsafe {
                (runtime.api.player_release)(player.as_ptr());
                (runtime.api.release)(instance.as_ptr());
                video_context.reset_after_stop();
                drop(Arc::from_raw(video_opaque));
            }
            return Err(VlcError::InitializationFailed(
                "libVLC could not monitor playback errors.".to_owned(),
            ));
        }

        Ok(Self {
            _video_context: video_context,
            video_opaque,
            event_manager,
            runtime,
            instance,
            player,
        })
    }

    pub fn version(&self) -> &str {
        &self.runtime.version
    }

    pub(super) fn equalizer_metadata(&self) -> (Vec<f32>, Vec<String>) {
        let api = self.runtime.api;
        let band_count = (unsafe { (api.equalizer_get_band_count)() } as usize).min(128);
        let bands = (0..band_count)
            .map(|index| unsafe { (api.equalizer_get_band_frequency)(index as c_uint) })
            .collect();
        let preset_count = (unsafe { (api.equalizer_get_preset_count)() } as usize).min(128);
        let presets = (0..preset_count)
            .map(|index| {
                let name = unsafe { (api.equalizer_get_preset_name)(index as c_uint) };
                if name.is_null() {
                    String::new()
                } else {
                    unsafe { CStr::from_ptr(name) }
                        .to_string_lossy()
                        .into_owned()
                }
            })
            .collect();
        (bands, presets)
    }

    pub(super) fn equalizer_preset_settings(
        &self,
        index: usize,
    ) -> Result<EqualizerSettings, VlcError> {
        let api = self.runtime.api;
        let preset_count = unsafe { (api.equalizer_get_preset_count)() } as usize;
        if index >= preset_count || index > c_uint::MAX as usize {
            return Err(VlcError::InvalidControl("equalizer preset"));
        }
        if unsafe { (api.equalizer_get_band_count)() } as usize != EQUALIZER_BAND_COUNT {
            return Err(VlcError::InvalidControl("equalizer band layout"));
        }
        let equalizer = NonNull::new(unsafe { (api.equalizer_new_from_preset)(index as c_uint) })
            .ok_or(VlcError::ControlFailed {
            action: "load the equalizer preset",
            message: "libVLC could not create the preset.".to_owned(),
        })?;
        let settings = EqualizerSettings {
            enabled: true,
            preamp_db: unsafe { (api.equalizer_get_preamp)(equalizer.as_ptr()) },
            bands_db: std::array::from_fn(|band| unsafe {
                (api.equalizer_get_amp_at_index)(equalizer.as_ptr(), band as c_uint)
            }),
            preset_index: Some(index),
        };
        unsafe { (api.equalizer_release)(equalizer.as_ptr()) };
        Ok(settings)
    }

    pub(super) fn set_equalizer_settings(
        &self,
        settings: &EqualizerSettings,
    ) -> Result<(), VlcError> {
        if !valid_equalizer_settings(settings) {
            return Err(VlcError::InvalidControl("equalizer gains"));
        }
        let api = self.runtime.api;
        let player = self.player.as_ptr();
        if !settings.enabled {
            if unsafe { (api.player_set_equalizer)(player, std::ptr::null_mut()) } != 0 {
                return Err(VlcError::ControlFailed {
                    action: "disable the equalizer",
                    message: "libVLC rejected the equalizer change.".to_owned(),
                });
            }
            return Ok(());
        }
        if unsafe { (api.equalizer_get_band_count)() } as usize != EQUALIZER_BAND_COUNT {
            return Err(VlcError::InvalidControl("equalizer band layout"));
        }
        let equalizer = if let Some(index) = settings.preset_index {
            if index >= unsafe { (api.equalizer_get_preset_count)() } as usize
                || index > c_uint::MAX as usize
            {
                return Err(VlcError::InvalidControl("equalizer preset"));
            }
            NonNull::new(unsafe { (api.equalizer_new_from_preset)(index as c_uint) })
        } else {
            NonNull::new(unsafe { (api.equalizer_new)() })
        }
        .ok_or(VlcError::ControlFailed {
            action: "enable the equalizer",
            message: "libVLC could not create the equalizer.".to_owned(),
        })?;

        let result = (|| {
            if unsafe { (api.equalizer_set_preamp)(equalizer.as_ptr(), settings.preamp_db) } != 0 {
                return Err(VlcError::ControlFailed {
                    action: "set the equalizer preamp",
                    message: "libVLC rejected the equalizer change.".to_owned(),
                });
            }
            for (index, gain_db) in settings.bands_db.iter().copied().enumerate() {
                if unsafe {
                    (api.equalizer_set_amp_at_index)(equalizer.as_ptr(), gain_db, index as c_uint)
                } != 0
                {
                    return Err(VlcError::ControlFailed {
                        action: "set an equalizer band",
                        message: "libVLC rejected the equalizer change.".to_owned(),
                    });
                }
            }
            if unsafe { (api.player_set_equalizer)(player, equalizer.as_ptr()) } != 0 {
                return Err(VlcError::ControlFailed {
                    action: "apply the equalizer",
                    message: "libVLC rejected the equalizer change.".to_owned(),
                });
            }
            Ok(())
        })();
        unsafe { (api.equalizer_release)(equalizer.as_ptr()) };
        result
    }

    pub(super) fn media_controls(&self) -> MediaControls {
        let player = self.player.as_ptr();
        let api = self.runtime.api;
        MediaControls {
            audio_tracks: copy_track_descriptions(
                unsafe { (api.audio_get_track_count)(player) },
                unsafe { (api.audio_get_track_description)(player) },
                api.track_description_list_release,
            ),
            subtitle_tracks: copy_track_descriptions(
                unsafe { (api.video_get_spu_count)(player) },
                unsafe { (api.video_get_spu_description)(player) },
                api.track_description_list_release,
            ),
            chapters: self.chapter_descriptions(),
            audio_track: unsafe { (api.audio_get_track)(player) },
            subtitle_track: unsafe { (api.video_get_spu)(player) },
            chapter: unsafe { (api.player_get_chapter)(player) },
            audio_delay_us: unsafe { (api.audio_get_delay)(player) },
            subtitle_delay_us: unsafe { (api.video_get_spu_delay)(player) },
        }
    }

    pub fn select_audio_track(&self, track_id: c_int) -> Result<(), VlcError> {
        if track_id < 0 {
            return Err(VlcError::InvalidControl("audio track"));
        }
        if unsafe { (self.runtime.api.audio_set_track)(self.player.as_ptr(), track_id) } != 0 {
            return Err(VlcError::ControlFailed {
                action: "select the audio track",
                message: self.runtime.last_error(),
            });
        }
        Ok(())
    }

    pub fn select_subtitle_track(&self, track_id: c_int) -> Result<(), VlcError> {
        if track_id < -1 {
            return Err(VlcError::InvalidControl("subtitle track"));
        }
        if unsafe { (self.runtime.api.video_set_spu)(self.player.as_ptr(), track_id) } != 0 {
            return Err(VlcError::ControlFailed {
                action: "select the subtitle track",
                message: self.runtime.last_error(),
            });
        }
        Ok(())
    }

    pub fn add_subtitle(&self, path: &Path) -> Result<(), VlcError> {
        let uri = subtitle_file_uri(path)?;
        if unsafe {
            (self.runtime.api.media_player_add_slave)(
                self.player.as_ptr(),
                SUBTITLE_SLAVE_TYPE,
                uri.as_ptr(),
                true,
            )
        } != 0
        {
            return Err(VlcError::ControlFailed {
                action: "add the subtitle file",
                message: self.runtime.last_error(),
            });
        }
        Ok(())
    }

    pub fn set_audio_delay(&self, delay_us: i64) -> Result<(), VlcError> {
        if !valid_media_delay(delay_us) {
            return Err(VlcError::InvalidControl("audio delay"));
        }
        if unsafe { (self.runtime.api.audio_set_delay)(self.player.as_ptr(), delay_us) } != 0 {
            return Err(VlcError::ControlFailed {
                action: "set the audio delay",
                message: self.runtime.last_error(),
            });
        }
        Ok(())
    }

    pub fn set_subtitle_delay(&self, delay_us: i64) -> Result<(), VlcError> {
        if !valid_media_delay(delay_us) {
            return Err(VlcError::InvalidControl("subtitle delay"));
        }
        if unsafe { (self.runtime.api.video_set_spu_delay)(self.player.as_ptr(), delay_us) } != 0 {
            return Err(VlcError::ControlFailed {
                action: "set the subtitle delay",
                message: self.runtime.last_error(),
            });
        }
        Ok(())
    }

    pub fn select_chapter(&self, chapter: c_int) -> Result<(), VlcError> {
        let count = unsafe { (self.runtime.api.player_get_chapter_count)(self.player.as_ptr()) };
        if chapter < 0 || chapter >= count {
            return Err(VlcError::InvalidControl("chapter"));
        }
        // libVLC 3 exposes no result code for chapter selection. The worker polls
        // the selected chapter afterward so the snapshot reflects VLC's value.
        unsafe { (self.runtime.api.player_set_chapter)(self.player.as_ptr(), chapter) };
        Ok(())
    }

    fn chapter_descriptions(&self) -> Vec<PlaybackTrack> {
        let player = self.player.as_ptr();
        let mut descriptions: *mut *mut LibVlcChapterDescription = std::ptr::null_mut();
        let count = unsafe {
            (self.runtime.api.player_get_full_chapter_descriptions)(player, -1, &mut descriptions)
        };
        if descriptions.is_null() {
            return Vec::new();
        }
        let _guard = ChapterDescriptionsGuard {
            descriptions,
            count: c_uint::try_from(count.max(0)).unwrap_or_default(),
            release: self.runtime.api.chapter_descriptions_release,
        };
        if count <= 0 {
            return Vec::new();
        }
        let mut chapters = Vec::with_capacity((count as usize).min(MAX_MEDIA_DESCRIPTIONS));
        for index in 0..(count as usize).min(MAX_MEDIA_DESCRIPTIONS) {
            let description = unsafe { *descriptions.add(index) };
            let name = if description.is_null() {
                String::new()
            } else {
                let name = unsafe { (*description).name };
                if name.is_null() {
                    String::new()
                } else {
                    unsafe { CStr::from_ptr(name) }
                        .to_string_lossy()
                        .trim()
                        .to_owned()
                }
            };
            chapters.push(PlaybackTrack {
                id: index as c_int,
                name: if name.is_empty() {
                    format!("Chapter {}", index + 1)
                } else {
                    name
                },
            });
        }
        chapters
    }

    pub fn stop(&self) {
        unsafe { (self.runtime.api.player_stop)(self.player.as_ptr()) };
        self._video_context.reset_after_stop();
    }

    pub fn stop_and_clear(&self) {
        self.stop_and_clear_internal();
    }

    pub(super) fn take_encountered_error(&self) -> bool {
        self._video_context.take_encountered_error()
    }

    pub fn set_paused(&self, paused: bool) {
        unsafe {
            (self.runtime.api.player_set_pause)(self.player.as_ptr(), i32::from(paused));
        }
    }

    #[cfg(test)]
    pub fn play(&mut self, track: &Track) -> Result<(), VlcError> {
        self.stop_and_clear();
        self.play_after_stop_to(track, None)
    }

    pub(super) fn play_after_stop_to(
        &mut self,
        track: &Track,
        cast_target: Option<&str>,
    ) -> Result<(), VlcError> {
        let (media, network_source) = match &track.source {
            TrackSource::Local => {
                if !track.path.is_file() {
                    return Err(VlcError::InvalidPath(track.path.clone()));
                }
                let path_string = track
                    .path
                    .to_str()
                    .ok_or_else(|| VlcError::PathNotUtf8(track.path.clone()))?;
                #[cfg(windows)]
                let path_string = windows_vlc_path(path_string)
                    .map_err(|_| VlcError::UnsupportedPathNamespace(track.path.clone()))?;
                #[cfg(not(windows))]
                let path_string = path_string.to_owned();
                let native_path = CString::new(path_string)
                    .map_err(|_| VlcError::PathNotUtf8(track.path.clone()))?;
                let media = NonNull::new(unsafe {
                    (self.runtime.api.media_new_path)(self.instance.as_ptr(), native_path.as_ptr())
                })
                .ok_or_else(|| VlcError::MediaCreationFailed(self.runtime.last_error()))?;
                (media, false)
            }
            TrackSource::Network(url) => {
                let location = CString::new(url.as_str())
                    .map_err(|_| VlcError::InvalidControl("network media URL"))?;
                let media = NonNull::new(unsafe {
                    (self.runtime.api.media_new_location)(self.instance.as_ptr(), location.as_ptr())
                })
                .ok_or_else(|| {
                    VlcError::MediaCreationFailed(
                        "libVLC could not create the network stream.".to_owned(),
                    )
                })?;
                (media, true)
            }
        };

        let cast_options = match cast_target.map(cast_media_options).transpose() {
            Ok(options) => options,
            Err(error) => {
                unsafe { (self.runtime.api.media_release)(media.as_ptr()) };
                return Err(error);
            }
        };
        self._video_context
            .encountered_error
            .store(false, Ordering::Release);

        unsafe {
            if !is_movie(track) {
                let no_video = CString::new(":no-video").expect("static option has no NUL byte");
                (self.runtime.api.media_add_option)(media.as_ptr(), no_video.as_ptr());
            }
            if let Some([demux_filter, output]) = cast_options {
                (self.runtime.api.media_add_option)(media.as_ptr(), demux_filter.as_ptr());
                (self.runtime.api.media_add_option)(media.as_ptr(), output.as_ptr());
            }
            (self.runtime.api.player_set_media)(self.player.as_ptr(), media.as_ptr());
            (self.runtime.api.media_release)(media.as_ptr());
        }

        if unsafe { (self.runtime.api.player_play)(self.player.as_ptr()) } != 0 {
            let message = self.runtime.last_error();
            self.stop_and_clear();
            return Err(VlcError::PlaybackFailed(if network_source {
                "libVLC could not start the network stream.".to_owned()
            } else {
                message
            }));
        }
        Ok(())
    }

    fn stop_and_clear_internal(&self) {
        unsafe {
            (self.runtime.api.player_stop)(self.player.as_ptr());
            (self.runtime.api.player_set_media)(self.player.as_ptr(), std::ptr::null_mut());
        }
        self._video_context.reset_after_stop();
    }

    pub fn toggle_pause(&self) -> Result<(), VlcError> {
        let state = self.state();
        if matches!(
            state,
            PlayerState::Stopped | PlayerState::Ended | PlayerState::Other
        ) {
            if unsafe { (self.runtime.api.player_play)(self.player.as_ptr()) } != 0 {
                return Err(VlcError::PlaybackFailed(self.runtime.last_error()));
            }
            return Ok(());
        }
        let should_pause = state != PlayerState::Paused;
        unsafe {
            (self.runtime.api.player_set_pause)(self.player.as_ptr(), i32::from(should_pause));
        }
        Ok(())
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

    pub fn is_seekable(&self) -> bool {
        unsafe { (self.runtime.api.player_is_seekable)(self.player.as_ptr()) != 0 }
    }

    pub fn seek(&self, position_ms: i64, duration_ms: i64) -> Result<i64, VlcError> {
        let position_ms =
            seek_target_ms(position_ms, duration_ms).ok_or(VlcError::InvalidControl("seek"))?;
        if !self.is_seekable() {
            return Err(VlcError::ControlFailed {
                action: "seek",
                message: "VLC reports that the current media is not seekable.".to_owned(),
            });
        }
        // libVLC 3 declares set_time as void; is_seekable is the available API check.
        unsafe { (self.runtime.api.player_set_time)(self.player.as_ptr(), position_ms) };
        Ok(position_ms)
    }

    pub fn set_volume(&self, volume_percent: i32) -> Result<(), VlcError> {
        if !valid_volume_percent(volume_percent) {
            return Err(VlcError::InvalidControl("volume"));
        }
        if unsafe { (self.runtime.api.audio_set_volume)(self.player.as_ptr(), volume_percent) } != 0
        {
            return Err(VlcError::ControlFailed {
                action: "change volume",
                message: self.runtime.last_error(),
            });
        }
        Ok(())
    }

    pub fn set_playback_rate(&self, rate: f32) -> Result<(), VlcError> {
        if !valid_playback_rate(rate) {
            return Err(VlcError::InvalidControl("playback rate"));
        }
        if unsafe { (self.runtime.api.player_set_rate)(self.player.as_ptr(), rate) } != 0 {
            return Err(VlcError::ControlFailed {
                action: "change playback rate",
                message: self.runtime.last_error(),
            });
        }
        Ok(())
    }

    pub fn error_message(&self) -> String {
        self.runtime.last_error()
    }
}

impl Drop for VlcEngine {
    fn drop(&mut self) {
        unsafe {
            // VLC 3 holds its event-manager lock while invoking callbacks; detach
            // waits for any in-flight error callback before the opaque Arc is freed.
            (self.runtime.api.event_detach)(
                self.event_manager.as_ptr(),
                LIBVLC_MEDIA_PLAYER_ENCOUNTERED_ERROR,
                Some(playback_event_callback),
                self.video_opaque.cast_mut().cast(),
            );
            (self.runtime.api.player_stop)(self.player.as_ptr());
            (self.runtime.api.player_release)(self.player.as_ptr());
            (self.runtime.api.release)(self.instance.as_ptr());
            self._video_context.reset_after_stop();
            drop(Arc::from_raw(self.video_opaque));
        }
        // `runtime` and its dynamic library are dropped after this method.
    }
}

#[cfg(test)]
mod tests {
    use super::{
        cast_media_options, copy_track_descriptions, is_libvlc_3, playback_event_callback,
        subtitle_file_uri, video_cleanup, video_display, video_lock, video_unlock,
        windows_vlc_path, LibVlcEvent, LibVlcTrackDescription, Runtime, VideoCallbackContext,
        VideoCallbackState, VlcEngine, VlcError, CAST_DEMUX_FILTER_OPTION,
        LIBVLC_MEDIA_PLAYER_ENCOUNTERED_ERROR,
    };
    use crate::model::{Track, TrackSource};
    use crate::platform::RuntimeCandidate;
    use crate::playback::frame::{FrameLayout, FrameMailbox, MAX_IN_FLIGHT_FRAMES};
    use crate::playback::queue::RepeatMode;
    use crate::playback::{PlaybackCommand, PlaybackUiState, PlaybackWorker};
    use std::f64::consts::TAU;
    use std::ffi::CString;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    const HTTP_404_CHILD_ENV: &str = "RUST_VLC_HTTP_404_SUBPROCESS";
    const SIGNED_URL_MARKER: &str = "RUST_VLC_SIGNED_QUERY_MARKER_6c8d2f";

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
    fn playback_error_callback_latches_only_the_libvlc_3024_error_event() {
        let context = callback_test_context(Arc::new(FrameMailbox::default()));
        let opaque = Arc::as_ptr(&context).cast_mut().cast();
        let harmless_event = LibVlcEvent { event_type: 0x109 };
        unsafe { playback_event_callback(&harmless_event, opaque) };
        assert!(!context.take_encountered_error());

        let error_event = LibVlcEvent {
            event_type: LIBVLC_MEDIA_PLAYER_ENCOUNTERED_ERROR,
        };
        assert_eq!(LIBVLC_MEDIA_PLAYER_ENCOUNTERED_ERROR, 0x10A);
        unsafe { playback_event_callback(&error_event, opaque) };
        assert!(context.take_encountered_error());
        assert!(!context.take_encountered_error());
    }

    #[test]
    fn cast_output_adds_the_required_cc_demux_filter() {
        let options = cast_media_options("192.168.1.25").expect("valid target should build");
        assert_eq!(options[0].to_str().unwrap(), CAST_DEMUX_FILTER_OPTION);
        assert_eq!(options[0].to_str().unwrap(), ":demux-filter=cc_demux");
        assert_eq!(
            options[1].to_str().unwrap(),
            ":sout=#chromecast{ip=192.168.1.25}"
        );
        assert!(cast_media_options("bad};:sout=#file").is_err());
    }

    #[test]
    #[ignore = "requires an installed libVLC 3 runtime"]
    fn installed_runtime_reports_and_applies_equalizer_settings() {
        let engine = VlcEngine::new(Arc::new(FrameMailbox::default()))
            .expect("installed libVLC 3 should initialize");
        let (frequencies, presets) = engine.equalizer_metadata();
        assert_eq!(frequencies.len(), super::super::EQUALIZER_BAND_COUNT);
        assert!(frequencies
            .iter()
            .all(|frequency| frequency.is_finite() && *frequency > 0.0));
        assert!(!presets.is_empty());

        let mut settings = engine
            .equalizer_preset_settings(0)
            .expect("libVLC should provide its first equalizer preset");
        engine
            .set_equalizer_settings(&settings)
            .expect("libVLC should apply the preset to the player");
        settings.preset_index = None;
        settings.bands_db[0] = 1.5;
        engine
            .set_equalizer_settings(&settings)
            .expect("libVLC should apply a custom band value");
        settings.enabled = false;
        engine
            .set_equalizer_settings(&settings)
            .expect("libVLC should detach the equalizer");
    }

    #[test]
    fn track_descriptions_are_copied_with_a_bound_and_the_full_list_is_freed() {
        let second_name = CString::new("Second audio").unwrap().into_raw();
        let second = Box::into_raw(Box::new(LibVlcTrackDescription {
            id: 9,
            name: second_name,
            next: std::ptr::null_mut(),
        }));
        let first = Box::into_raw(Box::new(LibVlcTrackDescription {
            id: 3,
            name: std::ptr::null_mut(),
            next: second,
        }));

        RELEASED_TEST_DESCRIPTIONS.store(0, Ordering::SeqCst);
        let tracks = copy_track_descriptions(1, first, release_test_descriptions);

        assert_eq!(
            tracks,
            [super::super::PlaybackTrack {
                id: 3,
                name: "Track 3".to_owned(),
            }]
        );
        assert_eq!(RELEASED_TEST_DESCRIPTIONS.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn subtitle_uri_round_trips_local_paths_with_uri_reserved_characters() {
        let path =
            std::env::temp_dir().join(format!("rust-vlc-subtitle # é-{}.srt", std::process::id()));
        fs::write(&path, "1\n00:00:00,000 --> 00:00:01,000\nHello\n")
            .expect("subtitle test file should be writable");
        let _temporary_file = TemporaryFile(path.clone());

        let uri = subtitle_file_uri(&path).expect("existing local subtitle should form a URI");
        let uri = unsafe { std::ffi::CStr::from_ptr(uri.as_ptr()) }
            .to_str()
            .expect("file URI should be valid UTF-8");
        let parsed = reqwest::Url::parse(uri).expect("subtitle URI should be parseable");
        assert_eq!(parsed.scheme(), "file");
        assert_eq!(parsed.to_file_path().unwrap(), path);
        assert!(parsed.as_str().contains("%20"));
        assert!(parsed.as_str().contains("%23"));
    }

    #[test]
    fn windows_vlc_path_converts_only_supported_verbatim_namespaces() {
        let ordinary_drive = r"C:\Users\Films\First Light.mp4";
        assert_eq!(windows_vlc_path(ordinary_drive).unwrap(), ordinary_drive);

        let ordinary_unc = r"\\media-server\Films\First Light.mp4";
        assert_eq!(windows_vlc_path(ordinary_unc).unwrap(), ordinary_unc);

        assert_eq!(
            windows_vlc_path(r"\\?\C:\Users\Films\Björk & First Light #1.mp4").unwrap(),
            r"C:\Users\Films\Björk & First Light #1.mp4"
        );
        assert_eq!(
            windows_vlc_path(r"\\?\UNC\media-server\Films\Björk & First Light.mp4").unwrap(),
            r"\\media-server\Films\Björk & First Light.mp4"
        );

        let long_tail = format!("C:\\Films\\{}feature.mp4", "long-folder-name\\".repeat(20));
        assert!(long_tail.len() > 260);
        let verbatim_long = format!(r"\\?\{long_tail}");
        assert_eq!(windows_vlc_path(&verbatim_long).unwrap(), long_tail);

        let drive_relative = r"C:Films\First Light.mp4";
        assert_eq!(windows_vlc_path(drive_relative).unwrap(), drive_relative);

        assert!(
            windows_vlc_path(r"\\?\Volume{12345678-1234-1234-1234-123456789012}\movie.mp4")
                .is_err()
        );
        assert!(windows_vlc_path(r"\\.\PhysicalDrive0").is_err());
        assert!(windows_vlc_path(r"\\?\UNC\media-server").is_err());
        assert!(windows_vlc_path(r"\\?\C:relative\movie.mp4").is_err());
    }

    static RELEASED_TEST_DESCRIPTIONS: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn release_test_descriptions(mut head: *mut LibVlcTrackDescription) {
        while !head.is_null() {
            let description = unsafe { Box::from_raw(head) };
            head = description.next;
            if !description.name.is_null() {
                drop(unsafe { CString::from_raw(description.name) });
            }
            RELEASED_TEST_DESCRIPTIONS.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn video_cleanup_waits_for_active_decoder_plane_before_releasing_it() {
        use std::ffi::c_void;
        use std::sync::{mpsc, Condvar, Mutex};

        let frames = Arc::new(FrameMailbox::default());
        let layout = FrameLayout::new(1, 1, 4).expect("tiny frame should be valid");
        let mut buffer = frames
            .allocate(layout, frames.generation())
            .expect("preallocated frame should fit the bounded pool");
        let plane = buffer.plane().cast::<c_void>();
        let context = Arc::new(VideoCallbackContext {
            frames,
            state: Mutex::new(VideoCallbackState {
                layout: Some(layout),
                buffers: vec![buffer],
                in_use: vec![true],
                picture_ids: vec![Some(99)],
                available: Vec::with_capacity(1),
                pending_frames: Vec::with_capacity(MAX_IN_FLIGHT_FRAMES),
                next_picture_id: Some(100),
                reconfiguring: false,
                shutting_down: false,
            }),
            buffer_returned: Condvar::new(),
            encountered_error: AtomicBool::new(false),
        });
        let opaque = Arc::into_raw(Arc::clone(&context))
            .cast_mut()
            .cast::<c_void>();
        let (finished_tx, finished_rx) = mpsc::channel();
        let opaque_address = opaque as usize;
        thread::spawn(move || {
            unsafe { video_cleanup(opaque_address as *mut c_void) };
            finished_tx
                .send(())
                .expect("test should still be waiting for cleanup");
        });

        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if context.lock_state().shutting_down {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "cleanup did not mark the callback context as shutting down"
            );
            thread::yield_now();
        }
        assert!(finished_rx.try_recv().is_err());

        unsafe { video_unlock(opaque, 99usize as *mut c_void, &plane) };
        finished_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("cleanup should finish after the decoder returns its plane");
        assert!(context.lock_state().buffers.is_empty());
        unsafe { drop(Arc::from_raw(opaque.cast::<VideoCallbackContext>())) };
    }

    #[test]
    fn video_unlock_recycles_plane_but_display_publishes_the_matching_frame() {
        use std::ffi::c_void;

        let frames = Arc::new(FrameMailbox::default());
        let context = callback_test_context(Arc::clone(&frames));
        let opaque = Arc::into_raw(Arc::clone(&context))
            .cast_mut()
            .cast::<c_void>();

        let mut first_plane = std::ptr::null_mut();
        let first_picture = unsafe { video_lock(opaque, &mut first_plane) };
        unsafe {
            std::ptr::copy_nonoverlapping([1_u8, 2, 3, 4].as_ptr(), first_plane.cast(), 4);
            video_unlock(opaque, first_picture, &first_plane);
        }
        assert!(
            frames.take_latest().is_none(),
            "unlock must not publish early"
        );
        assert_eq!(context.lock_state().available.len(), MAX_IN_FLIGHT_FRAMES);

        let mut second_plane = std::ptr::null_mut();
        let second_picture = unsafe { video_lock(opaque, &mut second_plane) };
        assert_ne!(first_picture, second_picture, "callback IDs must be unique");
        assert_eq!(first_plane, second_plane, "unlock should recycle its plane");
        unsafe {
            std::ptr::copy_nonoverlapping([5_u8, 6, 7, 8].as_ptr(), second_plane.cast(), 4);
            video_unlock(opaque, second_picture, &second_plane);
            video_display(opaque, first_picture);
        }
        assert_eq!(frames.take_latest().unwrap().pixels, [1, 2, 3, 4]);
        unsafe { video_display(opaque, second_picture) };
        assert_eq!(frames.take_latest().unwrap().pixels, [5, 6, 7, 8]);

        unsafe { video_cleanup(opaque) };
        assert_eq!(frames.in_flight(), 0);
        unsafe { drop(Arc::from_raw(opaque.cast::<VideoCallbackContext>())) };
    }

    #[test]
    fn media_reset_after_stop_discards_queued_old_picture_ids() {
        use std::ffi::c_void;

        let frames = Arc::new(FrameMailbox::default());
        let context = callback_test_context(Arc::clone(&frames));
        let opaque = Arc::into_raw(Arc::clone(&context))
            .cast_mut()
            .cast::<c_void>();

        let mut plane = std::ptr::null_mut();
        let picture = unsafe { video_lock(opaque, &mut plane) };
        unsafe {
            std::ptr::copy_nonoverlapping([1_u8, 2, 3, 4].as_ptr(), plane.cast(), 4);
            video_unlock(opaque, picture, &plane);
        }
        assert_eq!(context.lock_state().pending_frames.len(), 1);

        context.reset_after_stop();
        assert!(context.lock_state().pending_frames.is_empty());
        unsafe { video_display(opaque, picture) };
        assert!(
            frames.take_latest().is_none(),
            "a late display callback from the stopped media must be discarded"
        );

        unsafe { video_cleanup(opaque) };
        assert_eq!(frames.in_flight(), 0);
        unsafe { drop(Arc::from_raw(opaque.cast::<VideoCallbackContext>())) };
    }

    #[test]
    fn undisplayed_frames_are_bounded_and_cleanup_discards_their_ids_and_pixels() {
        use std::ffi::c_void;

        let frames = Arc::new(FrameMailbox::default());
        let context = callback_test_context(Arc::clone(&frames));
        let opaque = Arc::into_raw(Arc::clone(&context))
            .cast_mut()
            .cast::<c_void>();
        let mut pictures = Vec::new();

        for value in 1..=MAX_IN_FLIGHT_FRAMES + 1 {
            let mut plane = std::ptr::null_mut();
            let picture = unsafe { video_lock(opaque, &mut plane) };
            assert!(!picture.is_null());
            unsafe {
                std::ptr::write_bytes(plane.cast::<u8>(), value as u8, 4);
                video_unlock(opaque, picture, &plane);
            }
            pictures.push(picture);
        }
        assert_eq!(
            context.lock_state().pending_frames.len(),
            MAX_IN_FLIGHT_FRAMES,
            "never-displayed output must not grow pending storage"
        );
        assert!(frames.take_latest().is_none());

        unsafe { video_display(opaque, *pictures.last().unwrap()) };
        assert!(
            frames.take_latest().is_none(),
            "a dropped frame ID is ignored"
        );
        unsafe { video_cleanup(opaque) };
        assert!(context.lock_state().pending_frames.is_empty());
        assert_eq!(frames.in_flight(), 0);
        unsafe { video_display(opaque, pictures[0]) };
        assert!(
            frames.take_latest().is_none(),
            "cleanup discards pending display IDs"
        );
        unsafe { drop(Arc::from_raw(opaque.cast::<VideoCallbackContext>())) };
    }

    fn callback_test_context(frames: Arc<FrameMailbox>) -> Arc<VideoCallbackContext> {
        use std::sync::{Condvar, Mutex};

        let layout = FrameLayout::new(1, 1, 4).expect("tiny frame should be valid");
        let buffers = (0..MAX_IN_FLIGHT_FRAMES)
            .map(|_| {
                frames
                    .allocate(layout, frames.generation())
                    .expect("bounded frame pool should allocate")
            })
            .collect();
        let mut available = Vec::with_capacity(MAX_IN_FLIGHT_FRAMES);
        available.extend((0..MAX_IN_FLIGHT_FRAMES).rev());
        Arc::new(VideoCallbackContext {
            frames,
            state: Mutex::new(VideoCallbackState {
                layout: Some(layout),
                buffers,
                in_use: vec![false; MAX_IN_FLIGHT_FRAMES],
                picture_ids: vec![None; MAX_IN_FLIGHT_FRAMES],
                available,
                pending_frames: Vec::with_capacity(MAX_IN_FLIGHT_FRAMES),
                next_picture_id: Some(1),
                reconfiguring: false,
                shutting_down: false,
            }),
            buffer_returned: Condvar::new(),
            encountered_error: AtomicBool::new(false),
        })
    }

    #[test]
    #[ignore = "requires an installed libVLC 3 runtime and audio output"]
    fn installed_runtime_exercises_seek_volume_rate_and_stop() {
        let path = temporary_wave_path();
        write_test_wave(&path);
        let _temporary_file = TemporaryFile(path.clone());
        let track = Track::from_path(1, path).expect("generated WAV should be supported");
        let mut engine =
            VlcEngine::new(Arc::new(FrameMailbox::default())).expect("libVLC 3 should initialize");
        engine.play(&track).expect("generated WAV should play");

        let deadline = Instant::now() + Duration::from_secs(20);
        while engine.state() != super::PlayerState::Playing {
            assert_ne!(
                engine.state(),
                super::PlayerState::Error,
                "VLC reported an audio error: {}",
                engine.error_message()
            );
            assert!(Instant::now() < deadline, "WAV did not start playing");
            thread::sleep(Duration::from_millis(20));
        }

        let duration_ms = engine.length_ms();
        assert!(
            duration_ms >= 4_000,
            "unexpected WAV duration: {duration_ms}"
        );
        assert!(
            engine.is_seekable(),
            "VLC should report a local WAV as seekable"
        );
        let output_deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match engine.set_volume(35) {
                Ok(()) => break,
                Err(VlcError::ControlFailed {
                    action: "change volume",
                    message,
                }) if message
                    .to_ascii_lowercase()
                    .contains("no active audio output") =>
                {
                    assert!(
                        Instant::now() < output_deadline,
                        "VLC did not attach an audio output within 3 seconds"
                    );
                    thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("VLC should accept volume 35% after output is ready: {error}"),
            }
        }
        engine
            .set_playback_rate(1.25)
            .expect("VLC should accept playback speed 1.25x after output is ready");
        assert_eq!(
            engine
                .seek(1_500, duration_ms)
                .expect("VLC set_time should run"),
            1_500
        );

        let deadline = Instant::now() + Duration::from_secs(2);
        while engine.time_ms() < 1_000 {
            assert!(
                Instant::now() < deadline,
                "VLC did not reach the requested seek time"
            );
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            engine.time_ms() < 3_500,
            "VLC overshot the requested seek time"
        );

        engine.stop();
        let deadline = Instant::now() + Duration::from_secs(2);
        while engine.state() != super::PlayerState::Stopped {
            assert!(
                Instant::now() < deadline,
                "VLC did not enter the stopped state"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    #[ignore = "requires installed libVLC 3, video plugins, and ffmpeg"]
    fn installed_runtime_renders_a_generated_video_frame() {
        use std::process::Command;

        let path = temporary_video_path();
        let _temporary_file = TemporaryFile(path.clone());
        let blue_path =
            path.with_file_name(format!("rust-vlc-video-{}-blue.mp4", std::process::id()));
        let _blue_temporary_file = TemporaryFile(blue_path.clone());
        for (color, video_path) in [("red", &path), ("blue", &blue_path)] {
            let input = format!("color=c={color}:s=64x64:r=10:d=3");
            let generated = Command::new("ffmpeg")
                .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
                .arg(input)
                .args(["-c:v", "mpeg4", "-an", "-y"])
                .arg(video_path)
                .status()
                .expect("ffmpeg should be available for the ignored native smoke");
            assert!(
                generated.success(),
                "ffmpeg could not generate a {color} test movie"
            );
        }

        let mailbox = Arc::new(FrameMailbox::default());
        let mut engine =
            VlcEngine::new(Arc::clone(&mailbox)).expect("installed libVLC 3 should initialize");
        let path = path
            .canonicalize()
            .expect("generated video should canonicalize before playback");
        let blue_path = blue_path
            .canonicalize()
            .expect("generated blue video should canonicalize before playback");
        let tracks = [
            Track {
                id: 1,
                path: path.clone(),
                title: "Generated red movie".to_owned(),
                format: super::super::MOVIE_FORMAT.to_owned(),
                source: TrackSource::Local,
            },
            Track {
                id: 2,
                path: blue_path,
                title: "Generated blue movie".to_owned(),
                format: super::super::MOVIE_FORMAT.to_owned(),
                source: TrackSource::Local,
            },
        ];
        for iteration in 0..20 {
            let track = &tracks[iteration % tracks.len()];
            engine
                .play(track)
                .unwrap_or_else(|error| panic!("video reopen {iteration} failed: {error}"));

            let deadline = Instant::now() + Duration::from_secs(8);
            let frame = loop {
                if let Some(frame) = mailbox.take_latest() {
                    break frame;
                }
                assert_ne!(
                    engine.state(),
                    super::PlayerState::Error,
                    "VLC reported a video error during reopen {iteration}: {}",
                    engine.error_message()
                );
                assert!(
                    Instant::now() < deadline,
                    "libVLC produced no video frame during reopen {iteration}"
                );
                thread::sleep(Duration::from_millis(10));
            };
            assert_eq!((frame.width, frame.height), (64, 64));
            let pixels = frame.packed_rgba().expect("frame pitch should be valid");
            let center = ((frame.height as usize / 2 * frame.width as usize
                + frame.width as usize / 2)
                * 4) as usize;
            if track.id == 1 {
                assert!(
                    pixels[center] > pixels[center + 1] + 80,
                    "red channel was not first during reopen {iteration}"
                );
                assert!(
                    pixels[center] > pixels[center + 2] + 80,
                    "red channel was not first during reopen {iteration}"
                );
            } else {
                assert!(
                    pixels[center + 2] > pixels[center] + 80,
                    "blue channel was not last during reopen {iteration}"
                );
                assert!(
                    pixels[center + 2] > pixels[center + 1] + 80,
                    "blue channel was not last during reopen {iteration}"
                );
            }
            if iteration == 19 {
                mailbox.publish(frame);
            }
        }

        let missing_track = Track {
            id: 2,
            path: path.with_file_name(format!("missing-rust-vlc-test-{}.mkv", std::process::id())),
            title: "Missing replacement movie".to_owned(),
            format: super::super::MOVIE_FORMAT.to_owned(),
            source: TrackSource::Local,
        };
        assert!(!missing_track.path.exists());
        assert!(matches!(
            engine.play(&missing_track),
            Err(VlcError::InvalidPath(_))
        ));
        assert!(
            mailbox.take_latest().is_none(),
            "a failed replacement must still discard the old movie frame"
        );
        engine.stop();
    }

    #[test]
    #[ignore = "requires an installed libVLC 3 runtime and audio output"]
    fn delayed_seek_for_previous_track_does_not_seek_the_current_track() {
        let first_path = std::env::temp_dir().join(format!(
            "rust-vlc-audio-worker-{}-first.wav",
            std::process::id()
        ));
        let second_path = std::env::temp_dir().join(format!(
            "rust-vlc-audio-worker-{}-second.wav",
            std::process::id()
        ));
        write_test_wave(&first_path);
        write_test_wave(&second_path);
        let _first_file = TemporaryFile(first_path.clone());
        let _second_file = TemporaryFile(second_path.clone());
        let first = Track::from_path(1, first_path).expect("first WAV should be supported");
        let second = Track::from_path(2, second_path).expect("second WAV should be supported");
        let worker = PlaybackWorker::spawn().expect("playback worker should start");
        worker
            .submit(PlaybackCommand::PlayQueue {
                tracks: vec![first, second],
                start_index: 0,
                resume_position_ms: None,
            })
            .expect("worker should accept the test queue");

        wait_for_playing_track(&worker, 1);
        let first_generation = worker.snapshot().load_generation;
        worker
            .submit(PlaybackCommand::Next)
            .expect("worker should accept Next");
        wait_for_playing_track(&worker, 2);

        worker
            .submit(PlaybackCommand::SeekTo {
                request_id: 1,
                track_id: 1,
                load_generation: first_generation,
                position_ms: 4_000,
            })
            .expect("worker should accept the delayed seek command");

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let snapshot = worker.snapshot();
            if snapshot
                .message
                .contains("Seek was canceled because the current media changed")
            {
                assert_eq!(snapshot.current_id, Some(2));
                assert_eq!(snapshot.seek_ack.request_id, 1);
                assert!(!snapshot.seek_ack.succeeded);
                assert!(
                    snapshot.position_ms < 2_000,
                    "stale seek moved track B to {}ms",
                    snapshot.position_ms
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "worker did not cancel A's seek after advancing to B: {snapshot:?}"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    #[ignore = "requires an installed libVLC 3 runtime and audio output"]
    fn replaying_ended_track_reloads_it_and_repeat_all_advances_again() {
        let path = std::env::temp_dir().join(format!("rust-vlc-replay-{}.wav", std::process::id()));
        write_test_wave(&path);
        let _temporary_file = TemporaryFile(path.clone());
        let mut wave = fs::read(&path).expect("generated WAV should be readable");
        let data_size = 44_100_u32 * 2;
        wave.truncate(44 + data_size as usize);
        wave[4..8].copy_from_slice(&(36_u32 + data_size).to_le_bytes());
        wave[40..44].copy_from_slice(&data_size.to_le_bytes());
        fs::write(&path, wave).expect("short WAV should be writable");
        let first = Track::from_path(1, path.clone()).expect("generated WAV should be supported");
        let second = Track::from_path(2, path).expect("generated WAV should be supported");
        let worker = PlaybackWorker::spawn().expect("playback worker should start");
        worker
            .submit(PlaybackCommand::PlayQueue {
                tracks: vec![first, second],
                start_index: 1,
                resume_position_ms: None,
            })
            .expect("worker should accept the test queue");

        wait_for_playing_track(&worker, 2);
        let first_generation = worker.snapshot().load_generation;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let snapshot = worker.snapshot();
            if snapshot.current_id == Some(2) && snapshot.state == PlaybackUiState::Ended {
                assert_eq!(snapshot.queue_index, 2);
                assert_eq!(snapshot.load_generation, first_generation);
                break;
            }
            assert!(
                !matches!(
                    snapshot.state,
                    PlaybackUiState::Error | PlaybackUiState::Unavailable
                ),
                "initial WAV playback failed: {}",
                snapshot.message
            );
            assert!(
                Instant::now() < deadline,
                "initial WAV playback did not end: {snapshot:?}"
            );
            thread::sleep(Duration::from_millis(40));
        }

        worker
            .submit(PlaybackCommand::SetRepeat(RepeatMode::All))
            .expect("worker should accept Repeat All");
        worker
            .submit(PlaybackCommand::TogglePause)
            .expect("worker should accept replay");
        let deadline = Instant::now() + Duration::from_secs(20);
        let replay_generation = loop {
            let snapshot = worker.snapshot();
            if snapshot.current_id == Some(2)
                && snapshot.load_generation > first_generation
                && snapshot.state == PlaybackUiState::Playing
            {
                assert_eq!(snapshot.queue_index, 2);
                assert_eq!(snapshot.queue_len, 2);
                break snapshot.load_generation;
            }
            assert!(
                !matches!(
                    snapshot.state,
                    PlaybackUiState::Error | PlaybackUiState::Unavailable
                ),
                "replay failed: {}",
                snapshot.message
            );
            assert!(
                Instant::now() < deadline,
                "replay did not enter Playing: {snapshot:?}"
            );
            thread::sleep(Duration::from_millis(40));
        };

        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let snapshot = worker.snapshot();
            if snapshot.current_id == Some(1)
                && snapshot.load_generation > replay_generation
                && snapshot.state == PlaybackUiState::Playing
            {
                assert_eq!(snapshot.queue_index, 1);
                assert_eq!(snapshot.queue_len, 2);
                assert_eq!(snapshot.repeat_mode, RepeatMode::All);
                break;
            }
            assert!(
                !matches!(
                    snapshot.state,
                    PlaybackUiState::Error | PlaybackUiState::Unavailable
                ),
                "Repeat All failed to restart the track: {}",
                snapshot.message
            );
            assert!(
                Instant::now() < deadline,
                "Repeat All did not advance after replay ended: {snapshot:?}"
            );
            thread::sleep(Duration::from_millis(40));
        }
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
            source: TrackSource::Local,
        };
        let worker = PlaybackWorker::spawn().expect("playback worker should start");
        worker
            .submit(PlaybackCommand::PlayQueue {
                tracks: vec![track, missing_track],
                start_index: 0,
                resume_position_ms: None,
            })
            .expect("worker should accept the test queue");

        let deadline = Instant::now() + Duration::from_secs(20);
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

    #[test]
    #[ignore = "requires an installed libVLC 3 runtime"]
    fn native_http_404_worker_keeps_failed_track_and_never_logs_signed_url() {
        if std::env::var_os(HTTP_404_CHILD_ENV).is_some() {
            run_native_http_404_worker_child();
            return;
        }

        let executable = std::env::current_exe().expect("test executable path should be available");
        let output = std::process::Command::new(executable)
            .args([
                "--exact",
                "playback::ffi::tests::native_http_404_worker_keeps_failed_track_and_never_logs_signed_url",
                "--ignored",
                "--nocapture",
            ])
            .env(HTTP_404_CHILD_ENV, "1")
            .output()
            .expect("native VLC regression subprocess should start");
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stderr.contains(SIGNED_URL_MARKER),
            "libVLC wrote the signed URL marker to stderr"
        );
        assert!(
            !stdout.contains(SIGNED_URL_MARKER),
            "libVLC wrote the signed URL marker to stdout"
        );
        assert!(
            output.status.success(),
            "native HTTP 404 regression subprocess failed:\nstdout: {stdout}\nstderr: {stderr}"
        );
    }

    fn run_native_http_404_worker_child() {
        let worker = PlaybackWorker::spawn().expect("playback worker should start");
        let runtime_deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let snapshot = worker.snapshot();
            if snapshot.runtime_checked {
                assert!(
                    snapshot.runtime_available,
                    "native regression requires installed libVLC 3: {}",
                    snapshot.runtime_message
                );
                break;
            }
            assert!(
                Instant::now() < runtime_deadline,
                "libVLC runtime did not finish initializing within 120 seconds"
            );
            thread::sleep(Duration::from_millis(50));
        }
        let server = Http404Fixture::start();
        let first_url = format!("http://{}/first?token={SIGNED_URL_MARKER}", server.address);
        let second_url = format!("http://{}/second?token={SIGNED_URL_MARKER}", server.address);
        let first = Track::from_network_url(71, &first_url)
            .expect("the local signed HTTP URL should be accepted");
        let second = Track::from_network_url(72, &second_url)
            .expect("the second local HTTP URL should be accepted");
        worker
            .submit(PlaybackCommand::SetRepeat(RepeatMode::All))
            .expect("worker should accept Repeat All");
        worker
            .submit(PlaybackCommand::PlayQueue {
                tracks: vec![first, second],
                start_index: 0,
                resume_position_ms: None,
            })
            .expect("worker should accept the signed URL queue");

        let deadline = Instant::now() + Duration::from_secs(8);
        let initial_generation = loop {
            let snapshot = worker.snapshot();
            if snapshot.state == PlaybackUiState::Error && server.request_count() > 0 {
                assert_eq!(snapshot.current_id, Some(71));
                assert_eq!(snapshot.queue_index, 1);
                assert_eq!(snapshot.queue_len, 2);
                assert_eq!(snapshot.repeat_mode, RepeatMode::All);
                assert!(!snapshot.message.contains(SIGNED_URL_MARKER));
                assert!(!snapshot.message.contains("token="));
                break snapshot.load_generation;
            }
            assert!(
                Instant::now() < deadline,
                "HTTP 404 did not reach a stable playback error; snapshot: {snapshot:?}"
            );
            thread::sleep(Duration::from_millis(20));
        };

        thread::sleep(Duration::from_millis(1_000));
        let snapshot = worker.snapshot();
        assert_eq!(snapshot.current_id, Some(71));
        assert_eq!(snapshot.queue_index, 1);
        assert_eq!(snapshot.load_generation, initial_generation);
        assert_eq!(snapshot.state, PlaybackUiState::Error);
        assert!(
            server.request_count() > 0,
            "VLC did not request the local 404 URL"
        );
    }

    struct Http404Fixture {
        address: SocketAddr,
        requests: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl Http404Fixture {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0")
                .expect("local HTTP fixture should bind an ephemeral port");
            listener
                .set_nonblocking(true)
                .expect("local HTTP fixture should use nonblocking accept");
            let address = listener
                .local_addr()
                .expect("local HTTP fixture should report its address");
            let requests = Arc::new(AtomicUsize::new(0));
            let request_counter = Arc::clone(&requests);
            let stop = Arc::new(AtomicBool::new(false));
            let stop_server = Arc::clone(&stop);
            let server_thread = thread::spawn(move || {
                while !stop_server.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            request_counter.fetch_add(1, Ordering::Relaxed);
                            let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                            let mut request = [0_u8; 2048];
                            let _ = stream.read(&mut request);
                            let _ = stream.write_all(
                                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            );
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                address,
                requests,
                stop,
                thread: Some(server_thread),
            }
        }

        fn request_count(&self) -> usize {
            self.requests.load(Ordering::Relaxed)
        }
    }

    impl Drop for Http404Fixture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(server_thread) = self.thread.take() {
                let _ = server_thread.join();
            }
        }
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

    fn temporary_video_path() -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "rust-vlc-video-{}-{unique}.mp4",
            std::process::id()
        ))
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

    fn wait_for_playing_track(worker: &PlaybackWorker, track_id: i32) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let snapshot = worker.snapshot();
            if snapshot.current_id == Some(track_id) && snapshot.state == PlaybackUiState::Playing {
                return;
            }
            assert!(
                !matches!(
                    snapshot.state,
                    PlaybackUiState::Error | PlaybackUiState::Unavailable
                ),
                "libVLC did not play track {track_id}: {}",
                snapshot.message
            );
            assert!(
                Instant::now() < deadline,
                "track {track_id} did not start: {snapshot:?}"
            );
            thread::sleep(Duration::from_millis(40));
        }
    }
}
