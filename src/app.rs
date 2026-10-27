use crate::browse::{self, MusicGroup};
use crate::library::TrackSelection;
use crate::library_worker::{LibraryEvent, LibraryJob, LibraryWorker};
use crate::media_controls::{MediaControlBridge, MediaControlIntent};
use crate::metadata::{ArtworkKind, ArtworkRequest, LocalMetadata};
use crate::model::{readable_filename, MediaDetails, NetworkUrl, Track, TrackId, TrackSource};
use crate::playback::frame::FrameMailbox;
use crate::playback::queue::{QueueCheckpoint, RepeatMode};
use crate::playback::{
    send_command, AbPoint, EqualizerSettings, PlaybackCommand, PlaybackSnapshot, PlaybackUiState,
    PlaybackWorker, MAX_MEDIA_DELAY_US, MAX_PLAYBACK_RATE, MAX_VOLUME_PERCENT, MIN_PLAYBACK_RATE,
    MIN_VOLUME_PERCENT, MOVIE_FORMAT,
};
use crate::playlists::{PlaylistCollection, PlaylistId};
use crate::scan::ScanOutcome;
use crate::series::episode_info;
use crate::storage::{self, Bookmark, LibraryData, NetworkHistoryEntry, Progress, StorageWorker};
use crate::{
    AppWindow, BrowseRow, PictureInPictureWindow, QueueRow, TrackChoice, TrackRow, WatchedRootRow,
};
use rfd::FileDialog;
use slint::{
    CloseRequestResponse, ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer,
    SharedString, Timer, TimerMode, VecModel,
};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{mpsc::SyncSender, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TRANSPORT_DEBOUNCE: Duration = Duration::from_millis(220);
const TRANSPORT_ACK_TIMEOUT: Duration = Duration::from_secs(5);
const FULLSCREEN_CONTROLS_TIMEOUT: Duration = Duration::from_millis(2200);
const MUSIC_PAGE_SIZE: usize = 50;
const CARD_PAGE_SIZE: usize = 24;
const METADATA_BATCH_SIZE: usize = 32;
const MEDIA_CONTROL_HANDLE_RETRIES: u8 = 12;
const MAX_BOOKMARK_NAME_BYTES: usize = 128;
const MAX_NETWORK_HISTORY_TITLE_BYTES: usize = 256;
type PathIdentityKey = Vec<OsString>;
const PROGRESS_SAVE_INTERVAL: Duration = Duration::from_secs(5);
const MOVIE_DETAILS_CLICK_DELAY: Duration = Duration::from_millis(520);

struct PendingSeek {
    position_ms: i64,
    track_id: TrackId,
    load_generation: u64,
    changed_at: Instant,
    request_id: Option<u64>,
    submitted_at: Option<Instant>,
}

struct PendingValue<T> {
    value: T,
    changed_at: Instant,
    request_id: Option<u64>,
    submitted_at: Option<Instant>,
}

#[derive(Clone, Debug)]
struct DisplayedVideoFrame {
    width: u32,
    height: u32,
    rgba: Arc<[u8]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingSubtitleRequest {
    request_id: u64,
    media_id: TrackId,
    load_generation: u64,
}

struct AppState {
    tracks: Vec<Track>,
    movies: Vec<Track>,
    selection: TrackSelection,
    playlists: PlaylistCollection,
    saved_movies: HashSet<TrackId>,
    playlist_selection: HashSet<(PlaylistId, usize)>,
    details: HashMap<TrackId, MediaDetails>,
    progress: HashMap<TrackId, Progress>,
    query: String,
    new_playlist_name: String,
    current_view: String,
    music_section: String,
    open_music_group: Option<TrackId>,
    opened_playlist: Option<PlaylistId>,
    playlist_target: Option<PlaylistId>,
    active_playlist_owner: Option<ActivePlaylistOwner>,
    playlist_rename_input: String,
    movie_level: String,
    selected_series: Option<String>,
    selected_season: Option<u32>,
    movie_group_targets: HashMap<TrackId, MovieGroupTarget>,
    movie_filter: String,
    movie_view: String,
    movie_genre: String,
    movie_sort: String,
    music_genre: String,
    music_sort: String,
    music_sort_explicit: bool,
    movie_details_open: bool,
    selected_movie: Option<TrackId>,
    pending_movie_details: Option<(TrackId, Instant)>,
    artwork_target: Option<TrackId>,
    tmdb_token_input: String,
    tmdb_token_available: bool,
    settings_status: String,
    settings_error: bool,
    audio_delay_input: String,
    subtitle_delay_input: String,
    fullscreen_activity_at: Option<Instant>,
    jump_time_input: String,
    video_fill: bool,
    page_index: usize,
    visible_tracks: Vec<Track>,
    visible_playlist_positions: Vec<usize>,
    next_id: TrackId,
    feedback: String,
    library_status: String,
    watched_roots: Vec<PathBuf>,
    root_scan_status: HashMap<PathIdentityKey, String>,
    pending_root_scans: VecDeque<PathBuf>,
    active_root_scan: Option<PathBuf>,
    last_scheduled_root_scan: Instant,
    unavailable_media: HashSet<TrackId>,
    file_fingerprints: HashMap<PathIdentityKey, crate::scan::FileFingerprint>,
    playlist_io_status: String,
    queue_checkpoint: QueueCheckpoint,
    queue_restore_pending: bool,
    bookmarks: Vec<Bookmark>,
    network_history: Vec<NetworkHistoryEntry>,
    subtitle_results: Vec<crate::subtitles::SubtitleSearchResult>,
    subtitle_search_pending: Option<PendingSubtitleRequest>,
    subtitle_download_pending: Option<PendingSubtitleRequest>,
    subtitle_search_status: String,
    subtitle_language_input: String,
    next_subtitle_request_id: u64,
    equalizer_settings: EqualizerSettings,
    equalizer_settings_pending: bool,
    equalizer_pending_preset: Option<usize>,
    equalizer_restore_pending: bool,
    equalizer_editing: bool,
    equalizer_restore_sent_at: Option<Instant>,
    equalizer_restore_fallback: bool,
    equalizer_restore_failed: bool,
    bookmark_name_input: String,
    network_url_input: String,
    cast_address_input: String,
    storage: Option<StorageWorker>,
    library_worker: Option<LibraryWorker>,
    pending_metadata: VecDeque<TrackId>,
    automatic_artwork_readiness: HashMap<TrackId, bool>,
    automatic_artwork_cursor: usize,
    automatic_artwork_retries: VecDeque<TrackId>,
    active_automatic_artwork: Option<TrackId>,
    cache_dir: PathBuf,
    image_cache: VecDeque<(PathBuf, Image)>,
    next_artwork_generation: u64,
    artwork_intents: HashMap<TrackId, u64>,
    volume: i32,
    rate: f32,
    pending_seek: Option<PendingSeek>,
    pending_volume: Option<PendingValue<i32>>,
    pending_rate: Option<PendingValue<f32>>,
    next_control_request_id: u64,
    track_model: Rc<VecModel<TrackRow>>,
    episode_model: Rc<VecModel<TrackRow>>,
    music_card_model: Rc<VecModel<BrowseRow>>,
    movie_card_model: Rc<VecModel<BrowseRow>>,
    audio_model: Rc<VecModel<TrackChoice>>,
    subtitle_model: Rc<VecModel<TrackChoice>>,
    chapter_model: Rc<VecModel<TrackChoice>>,
    queue_model: Rc<VecModel<QueueRow>>,
    watched_roots_model: Rc<VecModel<WatchedRootRow>>,
    equalizer_band_model: Rc<VecModel<crate::EqualizerBandRow>>,
    equalizer_preset_model: Rc<VecModel<SharedString>>,
    playlist_targets_model: Rc<VecModel<SharedString>>,
    movie_genres_model: Rc<VecModel<SharedString>>,
    music_genres_model: Rc<VecModel<SharedString>>,
    rendered_queue_revision: u64,
    last_current_id: Option<TrackId>,
    last_current_load_generation: u64,
    last_network_history_key: Option<(TrackId, u64)>,
    bookmark_model: Rc<VecModel<crate::BookmarkRow>>,
    network_history_model: Rc<VecModel<crate::NetworkHistoryRow>>,
    subtitle_result_model: Rc<VecModel<crate::SubtitleResultRow>>,
    opensubtitles_credentials_available: bool,
    opensubtitles_status: String,
    media_controls_status: String,
}

#[derive(Clone, Debug)]
enum MovieGroupTarget {
    Series(String),
    Season(String, u32),
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            tracks: Vec::new(),
            movies: Vec::new(),
            selection: TrackSelection::default(),
            playlists: PlaylistCollection::default(),
            saved_movies: HashSet::new(),
            playlist_selection: HashSet::new(),
            details: HashMap::new(),
            progress: HashMap::new(),
            query: String::new(),
            new_playlist_name: String::new(),
            current_view: "movies".to_owned(),
            music_section: "albums".to_owned(),
            open_music_group: None,
            opened_playlist: None,
            playlist_target: None,
            active_playlist_owner: None,
            playlist_rename_input: String::new(),
            movie_level: "catalog".to_owned(),
            selected_series: None,
            selected_season: None,
            movie_group_targets: HashMap::new(),
            movie_filter: "all".to_owned(),
            movie_view: "all".to_owned(),
            movie_genre: "All genres".to_owned(),
            movie_sort: "recent".to_owned(),
            music_genre: "All genres".to_owned(),
            music_sort: "recent".to_owned(),
            music_sort_explicit: false,
            movie_details_open: false,
            selected_movie: None,
            pending_movie_details: None,
            artwork_target: None,
            tmdb_token_input: String::new(),
            tmdb_token_available: false,
            settings_status: String::new(),
            settings_error: false,
            audio_delay_input: "0".to_owned(),
            subtitle_delay_input: "0".to_owned(),
            fullscreen_activity_at: None,
            jump_time_input: String::new(),
            video_fill: false,
            page_index: 0,
            visible_tracks: Vec::new(),
            visible_playlist_positions: Vec::new(),
            next_id: 1,
            feedback: String::new(),
            library_status: String::new(),
            watched_roots: Vec::new(),
            root_scan_status: HashMap::new(),
            pending_root_scans: VecDeque::new(),
            active_root_scan: None,
            last_scheduled_root_scan: Instant::now(),
            unavailable_media: HashSet::new(),
            file_fingerprints: HashMap::new(),
            playlist_io_status: String::new(),
            queue_checkpoint: QueueCheckpoint::default(),
            queue_restore_pending: false,
            bookmarks: Vec::new(),
            network_history: Vec::new(),
            subtitle_results: Vec::new(),
            subtitle_search_pending: None,
            subtitle_download_pending: None,
            subtitle_search_status: String::new(),
            subtitle_language_input: "en".into(),
            next_subtitle_request_id: 1,
            equalizer_settings: EqualizerSettings::default(),
            equalizer_settings_pending: false,
            equalizer_pending_preset: None,
            equalizer_restore_pending: false,
            equalizer_editing: false,
            equalizer_restore_sent_at: None,
            equalizer_restore_fallback: false,
            equalizer_restore_failed: false,
            bookmark_name_input: String::new(),
            network_url_input: String::new(),
            cast_address_input: String::new(),
            storage: None,
            library_worker: None,
            pending_metadata: VecDeque::new(),
            automatic_artwork_readiness: HashMap::new(),
            automatic_artwork_cursor: 0,
            automatic_artwork_retries: VecDeque::new(),
            active_automatic_artwork: None,
            cache_dir: std::env::temp_dir().join("rust-vlc-cache"),
            image_cache: VecDeque::new(),
            next_artwork_generation: 1,
            artwork_intents: HashMap::new(),
            volume: 100,
            rate: 1.0,
            pending_seek: None,
            pending_volume: None,
            pending_rate: None,
            next_control_request_id: 1,
            track_model: Rc::new(VecModel::default()),
            episode_model: Rc::new(VecModel::default()),
            music_card_model: Rc::new(VecModel::default()),
            movie_card_model: Rc::new(VecModel::default()),
            audio_model: Rc::new(VecModel::default()),
            subtitle_model: Rc::new(VecModel::default()),
            chapter_model: Rc::new(VecModel::default()),
            queue_model: Rc::new(VecModel::default()),
            watched_roots_model: Rc::new(VecModel::default()),
            equalizer_band_model: Rc::new(VecModel::default()),
            equalizer_preset_model: Rc::new(VecModel::default()),
            playlist_targets_model: Rc::new(VecModel::default()),
            movie_genres_model: Rc::new(VecModel::default()),
            music_genres_model: Rc::new(VecModel::default()),
            rendered_queue_revision: u64::MAX,
            last_current_id: None,
            last_current_load_generation: 0,
            last_network_history_key: None,
            bookmark_model: Rc::new(VecModel::default()),
            network_history_model: Rc::new(VecModel::default()),
            subtitle_result_model: Rc::new(VecModel::default()),
            opensubtitles_credentials_available: false,
            opensubtitles_status: String::new(),
            media_controls_status: "Waiting for the app window to start.".into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ActivePlaylistOwner {
    playlist_id: PlaylistId,
    query: String,
    genre: String,
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let ui = AppWindow::new()?;
    let mut playback_worker = PlaybackWorker::spawn()?;
    let command_sender = playback_worker.command_sender();
    let snapshot = playback_worker.shared_snapshot();
    let video_frames = playback_worker.video_frames();
    let (mut app_state, startup_metadata) = load_app_state();
    app_state.queue_restore_pending = true;
    submit_queue_restore(&mut app_state, &command_sender, &snapshot);
    let startup_equalizer_settings = app_state.equalizer_settings;
    app_state.equalizer_settings_pending = true;
    app_state.equalizer_restore_pending = true;
    let state = Rc::new(RefCell::new(app_state));

    {
        let state = state.borrow();
        ui.set_tracks(ModelRc::from(Rc::clone(&state.track_model)));
        ui.set_movie_episodes(ModelRc::from(Rc::clone(&state.episode_model)));
        ui.set_music_cards(ModelRc::from(Rc::clone(&state.music_card_model)));
        ui.set_movie_cards(ModelRc::from(Rc::clone(&state.movie_card_model)));
        ui.set_audio_tracks(ModelRc::from(Rc::clone(&state.audio_model)));
        ui.set_subtitle_tracks(ModelRc::from(Rc::clone(&state.subtitle_model)));
        ui.set_chapters(ModelRc::from(Rc::clone(&state.chapter_model)));
        ui.set_queue_rows(ModelRc::from(Rc::clone(&state.queue_model)));
        ui.set_watched_roots(ModelRc::from(Rc::clone(&state.watched_roots_model)));
        ui.set_playlist_targets(ModelRc::from(Rc::clone(&state.playlist_targets_model)));
    }
    ui.set_app_version(SharedString::from(env!("CARGO_PKG_VERSION")));
    {
        let mut state = state.borrow_mut();
        state.pending_metadata.extend(startup_metadata);
        flush_pending_metadata(&mut state);
    }
    let initial = state.borrow();
    if initial.volume != 100 {
        let _ = send_command(
            &command_sender,
            PlaybackCommand::SetVolume {
                request_id: 1,
                volume_percent: initial.volume,
            },
            &snapshot,
        );
    }
    if (initial.rate - 1.0).abs() > f32::EPSILON {
        let _ = send_command(
            &command_sender,
            PlaybackCommand::SetPlaybackRate {
                request_id: 2,
                rate: initial.rate,
            },
            &snapshot,
        );
    }
    drop(initial);
    {
        let mut app_state = state.borrow_mut();
        app_state.equalizer_settings = startup_equalizer_settings;
        submit_equalizer_restore(&mut app_state, &command_sender, &snapshot);
    }

    refresh_from_state(&ui.as_weak(), &state, &snapshot);
    wire_library_ui(&ui, &state, &command_sender, &snapshot);
    wire_playlist_io(&ui, &state, &snapshot);
    let picture_in_picture = Rc::new(RefCell::new(None));
    let displayed_video_frame = Rc::new(RefCell::new(None));
    wire_picture_in_picture(
        &ui,
        &state,
        &snapshot,
        &picture_in_picture,
        &displayed_video_frame,
    );
    wire_extended_tools(&ui, &state, &command_sender, &snapshot);
    register_transport(&ui, &state, &command_sender, &snapshot);

    let weak = ui.as_weak();
    let state_timer = Rc::clone(&state);
    let sender_timer = command_sender.clone();
    let snapshot_timer = Arc::clone(&snapshot);
    let media_controls = Rc::new(RefCell::new(None::<MediaControlBridge>));
    let media_controls_attempts = Rc::new(Cell::new(0));
    let media_controls_timer = Rc::clone(&media_controls);
    let media_controls_attempts_timer = Rc::clone(&media_controls_attempts);
    let state_for_media_controls = Rc::clone(&state_timer);
    let sender_for_media_controls = sender_timer.clone();
    let snapshot_for_media_controls = Arc::clone(&snapshot_timer);
    let last_save = Rc::new(Cell::new(Instant::now()));
    let last_save_timer = Rc::clone(&last_save);
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_millis(180), move || {
        if let Some(ui) = weak.upgrade() {
            initialize_media_controls(
                &ui,
                &state_for_media_controls,
                &media_controls_timer,
                &media_controls_attempts_timer,
            );
            let intents = media_controls_timer
                .borrow()
                .as_ref()
                .map(MediaControlBridge::drain_events)
                .unwrap_or_default();
            apply_media_control_intents(
                &ui,
                &state_for_media_controls,
                &sender_for_media_controls,
                &snapshot_for_media_controls,
                intents,
            );
            let playback = read_snapshot(&snapshot_timer);
            {
                let mut state = state_timer.borrow_mut();
                submit_queue_restore(&mut state, &sender_timer, &snapshot_timer);
                submit_equalizer_restore(&mut state, &sender_timer, &snapshot_timer);
                flush_pending_transport(&ui, &mut state, &sender_timer, &snapshot_timer);
                process_library_events(&mut state, &playback, &sender_timer, &snapshot_timer);
                flush_pending_metadata(&mut state);
                schedule_automatic_artwork(&mut state);
                schedule_periodic_root_scans(&mut state);
                flush_pending_root_scans(&mut state);
                update_progress(&mut state, &playback);
                let equalizer_changed = reconcile_equalizer_settings(&mut state, &playback);
                let network_history_changed = update_network_history(&mut state, &playback);
                let periodic_save_due = last_save_timer.get().elapsed() >= PROGRESS_SAVE_INTERVAL;
                let queue_changed =
                    update_queue_checkpoint(&mut state, &playback, periodic_save_due);
                if periodic_save_due
                    || queue_changed
                    || equalizer_changed
                    || network_history_changed
                {
                    persist_library(&mut state);
                    last_save_timer.set(Instant::now());
                }
                if let Some(storage) = state.storage.as_ref() {
                    if let Some(error) = storage.take_error() {
                        state.library_status = format!("Library save failed: {error}");
                        state.feedback =
                            "The library could not be saved. Changes remain in memory.".into();
                    }
                }
                refresh_ui(&ui, &mut state, &playback);
            }

            let playback = read_snapshot(&snapshot_timer);
            let details = playback
                .current_id
                .and_then(|id| state_timer.borrow().details.get(&id).cloned());
            let artwork = details.as_ref().and_then(|details| details.artwork.clone());
            let mut media_controls = media_controls_timer.borrow_mut();
            if let Some(bridge) = media_controls.as_mut() {
                if let Err(error) = bridge.update(&playback, details.as_ref(), artwork.as_deref())
                {
                    let status = format!(
                        "OS media controls could not be updated: {error}. Local playback remains available."
                    );
                    let mut state = state_timer.borrow_mut();
                    if state.media_controls_status != status {
                        state.media_controls_status = status.clone();
                        ui.set_media_controls_status(SharedString::from(status));
                    }
                }
            }
        }
    });

    let video_weak = ui.as_weak();
    let frames_timer = Arc::clone(&video_frames);
    let video_snapshot = Arc::clone(&snapshot);
    let picture_in_picture_timer = Rc::clone(&picture_in_picture);
    let displayed_video_frame_timer = Rc::clone(&displayed_video_frame);
    let displayed_generation = Rc::new(Cell::new(video_frames.generation()));
    let frame_timer = Timer::default();
    frame_timer.start(TimerMode::Repeated, Duration::from_millis(16), move || {
        if let Some(ui) = video_weak.upgrade() {
            let playback = read_snapshot(&video_snapshot);
            refresh_video_frame(
                &ui,
                &frames_timer,
                &displayed_generation,
                &playback,
                &picture_in_picture_timer,
                &displayed_video_frame_timer,
            );
        }
    });

    let result = ui.run();
    timer.stop();
    frame_timer.stop();
    if let Some(picture_in_picture) = picture_in_picture.borrow().as_ref() {
        let _ = picture_in_picture.hide();
    }
    let storage_error = {
        let mut state = state.borrow_mut();
        let playback = read_snapshot(&snapshot);
        process_library_events(&mut state, &playback, &command_sender, &snapshot);
        update_progress(&mut state, &playback);
        update_queue_checkpoint(&mut state, &playback, true);
        persist_library(&mut state);
        if let Some(mut worker) = state.library_worker.take() {
            worker.shutdown();
        }
        if let Some(mut storage) = state.storage.take() {
            storage.shutdown();
            storage.take_error()
        } else {
            None
        }
    };
    playback_worker.shutdown();
    result?;
    if let Some(error) = storage_error {
        return Err(format!("Library save failed while closing: {error}").into());
    }
    Ok(())
}

fn initialize_media_controls(
    ui: &AppWindow,
    state: &Rc<RefCell<AppState>>,
    bridge: &Rc<RefCell<Option<MediaControlBridge>>>,
    attempts: &Cell<u8>,
) {
    if bridge.borrow().is_some() || attempts.get() >= MEDIA_CONTROL_HANDLE_RETRIES {
        return;
    }
    let attempt = attempts.get().saturating_add(1);
    attempts.set(attempt);
    match MediaControlBridge::new(ui.window()) {
        Ok(controls) => {
            *bridge.borrow_mut() = Some(controls);
            set_media_controls_status(ui, state, "OS media controls are active.".into());
        }
        Err(error)
            if error.contains("window handle is not available yet")
                && attempt < MEDIA_CONTROL_HANDLE_RETRIES =>
        {
            set_media_controls_status(ui, state, "Waiting for the app window to start.".into());
        }
        Err(error) => {
            attempts.set(MEDIA_CONTROL_HANDLE_RETRIES);
            set_media_controls_status(
                ui,
                state,
                format!(
                    "OS media controls are unavailable: {error}. Local playback remains available."
                ),
            );
        }
    }
}

fn set_media_controls_status(ui: &AppWindow, state: &Rc<RefCell<AppState>>, status: String) {
    let mut state = state.borrow_mut();
    if state.media_controls_status != status {
        state.media_controls_status = status.clone();
        ui.set_media_controls_status(SharedString::from(status));
    }
}

fn apply_media_control_intents(
    ui: &AppWindow,
    state: &Rc<RefCell<AppState>>,
    sender: &SyncSender<PlaybackCommand>,
    snapshot: &Arc<Mutex<PlaybackSnapshot>>,
    intents: Vec<MediaControlIntent>,
) {
    for intent in intents {
        match intent {
            MediaControlIntent::Play => {
                set_media_controls_paused(ui, state, sender, snapshot, false);
            }
            MediaControlIntent::Pause => {
                set_media_controls_paused(ui, state, sender, snapshot, true);
            }
            MediaControlIntent::Toggle => {
                if can_toggle_pause(&read_snapshot(snapshot)) {
                    ui.invoke_toggle_pause();
                }
            }
            MediaControlIntent::Stop => {
                let initial = read_snapshot(snapshot);
                if media_control_transport_is_current(snapshot, &initial) {
                    ui.invoke_stop();
                }
            }
            MediaControlIntent::Next => {
                let initial = read_snapshot(snapshot);
                if media_control_transport_is_current(snapshot, &initial) {
                    ui.invoke_next();
                }
            }
            MediaControlIntent::Previous => {
                let initial = read_snapshot(snapshot);
                if media_control_transport_is_current(snapshot, &initial) {
                    ui.invoke_previous();
                }
            }
            MediaControlIntent::SeekRelative(offset_ms) => {
                seek_from_media_controls(ui, snapshot, None, offset_ms);
            }
            MediaControlIntent::SeekTo(position_ms) => {
                seek_from_media_controls(ui, snapshot, Some(position_ms), 0);
            }
            MediaControlIntent::SetVolume(volume) => {
                ui.invoke_volume_changed(volume.clamp(0, 100) as f32, true);
            }
        }
    }
}

fn set_media_controls_paused(
    ui: &AppWindow,
    state: &Rc<RefCell<AppState>>,
    sender: &SyncSender<PlaybackCommand>,
    snapshot: &Arc<Mutex<PlaybackSnapshot>>,
    paused: bool,
) {
    let observed = read_snapshot(snapshot);
    if !media_control_pause_change_needed(&observed, paused) {
        return;
    }
    let playback = read_snapshot(snapshot);
    if !media_control_snapshot_matches(&observed, &playback)
        || !media_control_pause_change_needed(&playback, paused)
    {
        return;
    }

    let mut state = state.borrow_mut();
    update_progress(&mut state, &playback);
    submit_transport_command(
        &mut state,
        sender,
        snapshot,
        PlaybackCommand::SetPaused { paused },
    );
    persist_library(&mut state);
    refresh_from_parts(&ui.as_weak(), &mut state, snapshot);
}

fn media_control_pause_change_needed(playback: &PlaybackSnapshot, _paused: bool) -> bool {
    playback.queue_len > 0
        && match playback.state {
            PlaybackUiState::Loading | PlaybackUiState::Playing | PlaybackUiState::Paused => true,
            PlaybackUiState::Idle | PlaybackUiState::Ended => true,
            PlaybackUiState::Error | PlaybackUiState::Unavailable => false,
        }
}

fn media_control_snapshot_matches(observed: &PlaybackSnapshot, current: &PlaybackSnapshot) -> bool {
    observed.current_id == current.current_id
        && observed.load_generation == current.load_generation
        && observed.queue_revision == current.queue_revision
        && observed.queue_len == current.queue_len
}

fn media_control_transport_is_current(
    snapshot: &Arc<Mutex<PlaybackSnapshot>>,
    observed: &PlaybackSnapshot,
) -> bool {
    let current = read_snapshot(snapshot);
    media_control_snapshot_matches(observed, &current)
        && current.current_id.is_some()
        && current.queue_len > 0
        && matches!(
            current.state,
            PlaybackUiState::Loading
                | PlaybackUiState::Playing
                | PlaybackUiState::Paused
                | PlaybackUiState::Ended
        )
}

fn seek_from_media_controls(
    ui: &AppWindow,
    snapshot: &Arc<Mutex<PlaybackSnapshot>>,
    absolute_position_ms: Option<i64>,
    relative_offset_ms: i64,
) {
    let observed = read_snapshot(snapshot);
    let Some(track_id) = observed
        .current_id
        .filter(|_| playback_is_seekable(&observed))
    else {
        return;
    };
    let current = read_snapshot(snapshot);
    if !media_control_snapshot_matches(&observed, &current) || !playback_is_seekable(&current) {
        return;
    }
    let position_ms = absolute_position_ms
        .unwrap_or_else(|| current.position_ms.saturating_add(relative_offset_ms))
        .clamp(0, current.duration_ms);
    ui.invoke_seek_changed(
        position_ms as f32,
        track_id,
        SharedString::from(current.load_generation.to_string()),
        true,
    );
}

fn load_app_state() -> (AppState, Vec<TrackId>) {
    let mut state = AppState::default();
    match crate::credentials::has_tmdb_token() {
        Ok(saved) => {
            state.tmdb_token_available = saved;
            state.settings_status = if saved {
                "A TMDB token is available for movie artwork lookup.".into()
            } else {
                "No TMDB token is configured.".into()
            };
        }
        Err(error) => {
            state.settings_error = true;
            state.settings_status = format!("Could not check the saved TMDB token: {error}");
        }
    }
    match crate::credentials::has_opensubtitles_credentials() {
        Ok(saved) => {
            state.opensubtitles_credentials_available = saved;
            state.opensubtitles_status = if saved {
                "OpenSubtitles credentials are stored securely. Enter new values to replace them."
                    .into()
            } else {
                "Add an OpenSubtitles API key to search, and a bearer token to download.".into()
            };
        }
        Err(error) => {
            state.opensubtitles_status =
                format!("Could not check OpenSubtitles credentials: {error}");
        }
    }
    let mut loaded = LibraryData::default();
    match storage::application_directories() {
        Ok((database, cache)) => {
            state.cache_dir = cache.clone();
            match StorageWorker::spawn(database) {
                Ok((worker, data)) => {
                    state.storage = Some(worker);
                    loaded = data;
                    state.library_status =
                        "Library loaded from this device. Metadata and artwork stay local.".into();
                }
                Err(error) => {
                    state.library_status =
                        format!("Memory-only library. Existing database was preserved: {error}")
                }
            }
        }
        Err(error) => {
            state.library_status =
                format!("Memory-only library. App data directory is unavailable: {error}");
            state.cache_dir = std::env::temp_dir().join("rust-vlc-cache");
        }
    }
    let _ = std::fs::create_dir_all(&state.cache_dir);
    state.tracks = loaded.tracks;
    state.movies = loaded.movies;
    state.saved_movies = loaded.saved_movie_ids.into_iter().collect();
    state.playlists = PlaylistCollection::restore(loaded.playlists, loaded.selected_playlist);
    state.playlist_target = state.playlists.selected_id();
    state.current_view = loaded.current_view;
    state.volume = loaded.volume.clamp(MIN_VOLUME_PERCENT, MAX_VOLUME_PERCENT);
    state.rate = if loaded.rate.is_finite() {
        loaded.rate.clamp(MIN_PLAYBACK_RATE, MAX_PLAYBACK_RATE)
    } else {
        1.0
    };
    state.progress = loaded
        .progress
        .into_iter()
        .map(|progress| (progress.media_id, progress))
        .collect();
    state.details = loaded.details.into_iter().collect();
    state.watched_roots = loaded.watched_roots;
    state.queue_checkpoint = loaded.queue_checkpoint;
    state.bookmarks = loaded.bookmarks;
    state.network_history = loaded.network_history;
    state.equalizer_settings = loaded.equalizer_settings;
    for root in &state.watched_roots {
        state
            .root_scan_status
            .insert(path_identity_key(root), "Queued".into());
        state.pending_root_scans.push_back(root.clone());
    }
    state.next_id = state
        .tracks
        .iter()
        .chain(&state.movies)
        .map(|track| track.id)
        .max()
        .unwrap_or(0)
        .saturating_add(1)
        .max(1);
    let metadata = state
        .tracks
        .iter()
        .chain(&state.movies)
        .filter(|track| track.source == TrackSource::Local)
        .map(|track| track.id)
        .collect();
    if let Ok(worker) = LibraryWorker::spawn(state.cache_dir.clone()) {
        state.library_worker = Some(worker);
    } else {
        state
            .library_status
            .push_str(" Artwork and folder scanning are unavailable.");
        for status in state.root_scan_status.values_mut() {
            *status = "Scanning unavailable".into();
        }
        state.pending_root_scans.clear();
    }
    (state, metadata)
}

fn wire_library_ui(
    ui: &AppWindow,
    app_state: &Rc<RefCell<AppState>>,
    sender: &SyncSender<PlaybackCommand>,
    snapshot: &Arc<Mutex<PlaybackSnapshot>>,
) {
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_navigate(move |view| {
            let mut state = state.borrow_mut();
            state.pending_movie_details = None;
            state.tmdb_token_input.clear();
            state.current_view = if view.as_str() == "movies" {
                "movies"
            } else {
                "music"
            }
            .into();
            state.page_index = 0;
            if let Some(ui) = weak.upgrade() {
                ui.set_settings_open(false);
                ui.set_tmdb_token_input(SharedString::default());
            }
            persist_library(&mut state);
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        ui.on_tmdb_token_input_changed(move |input| {
            state.borrow_mut().tmdb_token_input = input.to_string();
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_save_tmdb_token(move || {
            let mut state = state.borrow_mut();
            match crate::credentials::save_tmdb_token(&state.tmdb_token_input) {
                Ok(()) => {
                    state.tmdb_token_input.clear();
                    state.tmdb_token_available = true;
                    state.settings_error = false;
                    state.settings_status =
                        "TMDB token saved in the system credential store.".into();
                }
                Err(error) => {
                    state.settings_error = true;
                    state.settings_status = error;
                }
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_remove_tmdb_token(move || {
            let mut state = state.borrow_mut();
            match crate::credentials::clear_tmdb_token() {
                Ok(()) => {
                    state.tmdb_token_input.clear();
                    state.settings_error = false;
                    match crate::credentials::has_tmdb_token() {
                        Ok(available) => {
                            state.tmdb_token_available = available;
                            state.settings_status = if available {
                                "Stored token removed. TMDB_READ_TOKEN is still available.".into()
                            } else {
                                "Stored TMDB token removed.".into()
                            };
                        }
                        Err(error) => {
                            state.tmdb_token_available = false;
                            state.settings_error = true;
                            state.settings_status = format!("Stored token removed, but availability could not be checked: {error}");
                        }
                    }
                }
                Err(error) => {
                    state.settings_error = true;
                    state.settings_status = error;
                }
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_settings_closed(move || {
            let mut state = state.borrow_mut();
            state.tmdb_token_input.clear();
            if let Some(ui) = weak.upgrade() {
                ui.set_settings_open(false);
                ui.set_tmdb_token_input(SharedString::default());
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_music_section_changed(move |section| {
            let mut state = state.borrow_mut();
            state.music_section = section.to_string();
            if section == "albums" || state.music_sort == "album" {
                state.music_sort = "recent".into();
                state.music_sort_explicit = false;
            }
            state.open_music_group = None;
            state.opened_playlist = None;
            state.playlist_rename_input.clear();
            state.page_index = 0;
            state.feedback.clear();
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_back_music_detail(move || {
            let mut state = state.borrow_mut();
            if state.music_sort == "album" {
                state.music_sort = "recent".into();
                state.music_sort_explicit = false;
            }
            state.open_music_group = None;
            state.opened_playlist = None;
            state.playlist_rename_input.clear();
            state.page_index = 0;
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_open_music_group(move |id| {
            let mut state = state.borrow_mut();
            if state.music_section == "albums" {
                state.music_sort = "recent".into();
                state.music_sort_explicit = false;
            }
            state.open_music_group = Some(id);
            state.artwork_target = Some(id);
            state.page_index = 0;
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_open_playlist(move |id| {
            let mut state = state.borrow_mut();
            state.opened_playlist = Some(id);
            state.open_music_group = None;
            state.playlist_rename_input = state
                .playlists
                .playlists()
                .iter()
                .find(|playlist| playlist.id == id)
                .map(|playlist| playlist.name.clone())
                .unwrap_or_default();
            state.page_index = 0;
            state.artwork_target = state
                .playlists
                .playlists()
                .iter()
                .find(|p| p.id == id)
                .and_then(|p| p.track_ids.first().copied());
            if let Some(ui) = weak.upgrade() {
                ui.set_confirm_delete_playlist(false);
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let sender = sender.clone();
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_shuffle_opened_playlist(move || {
            let mut state = state.borrow_mut();
            if let Some(id) = state.opened_playlist {
                let name = state
                    .playlists
                    .playlists()
                    .iter()
                    .find(|p| p.id == id)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| "playlist".into());
                let active_owner = state
                    .active_playlist_owner
                    .clone()
                    .filter(|owner| owner.playlist_id == id);
                match state.playlists.shuffle_playlist(id) {
                    Ok(()) => {
                        state
                            .playlist_selection
                            .retain(|(playlist_id, _)| *playlist_id != id);
                        persist_library(&mut state);
                        if let Some(owner) = active_owner {
                            let tracks = playlist_collection_for(&state, id, &owner.query, &owner.genre);
                            let revision = read_snapshot(&snapshot).queue_revision;
                            match send_command(
                                &sender,
                                PlaybackCommand::RefreshOwningQueue { revision, tracks },
                                &snapshot,
                            ) {
                                Ok(()) => state.feedback.clear(),
                                Err(error) => state.feedback = format!("Shuffled {name}, but the active queue was left unchanged: {error}"),
                            }
                        } else {
                            state.feedback = format!("Shuffled the stored order of {name}.");
                        }
                    }
                    Err(error) => state.feedback = error.to_string(),
                }
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_select_target_playlist(move |name| {
            let mut state = state.borrow_mut();
            state.playlist_target = state
                .playlists
                .playlists()
                .iter()
                .find(|p| p.name == name.as_str())
                .map(|p| p.id);
            if let Some(id) = state.playlist_target {
                state.playlists.select(id);
            }
            persist_library(&mut state);
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_playlist_name_changed(move |name| {
            let mut state = state.borrow_mut();
            state.new_playlist_name = name.to_string();
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_create_playlist(move |name| {
            let mut state = state.borrow_mut();
            match state.playlists.create(name.as_str()) {
                Ok(id) => {
                    state.new_playlist_name.clear();
                    state.playlist_target = Some(id);
                    state.feedback = "Playlist created and saved on this device.".into();
                    persist_library(&mut state);
                }
                Err(error) => state.feedback = error.to_string(),
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_create_playlist_from_selection(move |name, allow_duplicates| {
            let mut state = state.borrow_mut();
            let selected = selected_displayed_tracks(&state);
            if selected.is_empty() {
                state.feedback = "Check one or more tracks on this page first.".into();
            } else {
                match state.playlists.create(name.as_str()) {
                    Ok(id) => {
                        state.playlist_target = Some(id);
                        match state
                            .playlists
                            .add_tracks_with_duplicates(&selected, allow_duplicates)
                        {
                            Ok(count) => {
                                state.new_playlist_name.clear();
                                state.feedback = format!(
                                    "Created playlist and added {count} checked track occurrence(s)."
                                );
                                if state.current_view == "music"
                                    && state.music_section == "playlists"
                                    && state.opened_playlist.is_some()
                                {
                                    if let Some(playlist_id) = state.opened_playlist {
                                        for position in state.visible_playlist_positions.clone() {
                                            state.playlist_selection.remove(&(playlist_id, position));
                                        }
                                    }
                                } else {
                                    for track in state.visible_tracks.clone() {
                                        state.selection.set(track.id, false);
                                    }
                                }
                                persist_library(&mut state);
                            }
                            Err(error) => {
                                state.playlists.remove(id);
                                state.playlist_target = state.playlists.selected_id();
                                state.feedback = error.to_string();
                            }
                        }
                    }
                    Err(error) => state.feedback = error.to_string(),
                }
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_add_selected_to_playlist(move |allow_duplicates| {
            let mut state = state.borrow_mut();
            let selected = selected_displayed_tracks(&state);
            if let Some(target) = state.playlist_target {
                state.playlists.select(target);
            }
            match state
                .playlists
                .add_tracks_with_duplicates(&selected, allow_duplicates)
            {
                Ok(0) => {
                    state.feedback = "No new tracks were added to the selected playlist.".into()
                }
                Ok(count) => {
                    let name = state
                        .playlist_target
                        .and_then(|id| state.playlists.playlists().iter().find(|p| p.id == id))
                        .map(|p| p.name.as_str())
                        .unwrap_or("playlist");
                    state.feedback =
                        format!("Added {count} checked track occurrence(s) to {name}.");
                    if state
                        .active_playlist_owner
                        .as_ref()
                        .is_some_and(|owner| Some(owner.playlist_id) == state.playlist_target)
                    {
                        state.active_playlist_owner = None;
                    }
                    persist_library(&mut state);
                }
                Err(error) => state.feedback = error.to_string(),
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_select_all(move |selected| {
            let mut state = state.borrow_mut();
            let opened_playlist =
                if state.current_view == "music" && state.music_section == "playlists" {
                    state.opened_playlist
                } else {
                    None
                };
            if let Some(playlist_id) = opened_playlist {
                let positions = state.visible_playlist_positions.clone();
                for position in positions {
                    if selected {
                        state.playlist_selection.insert((playlist_id, position));
                    } else {
                        state.playlist_selection.remove(&(playlist_id, position));
                    }
                }
            } else {
                for track in state.visible_tracks.clone() {
                    state.selection.set(track.id, selected);
                }
            }
            state.feedback.clear();
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_toggle_track(move |id, selected| {
            let mut state = state.borrow_mut();
            state.selection.set(id, selected);
            state.artwork_target = Some(id);
            state.feedback.clear();
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_toggle_playlist_entry(
            move |playlist_id, display_index, position, track_id, selected| {
                let mut state = state.borrow_mut();
                if playlist_row_is_current(&state, playlist_id, display_index, position, track_id) {
                    let key = (playlist_id, position as usize);
                    if selected {
                        state.playlist_selection.insert(key);
                    } else {
                        state.playlist_selection.remove(&key);
                    }
                    state.artwork_target = Some(track_id);
                    state.feedback.clear();
                } else {
                    state.feedback = "That playlist row is stale. Refresh and try again.".into();
                }
                refresh_from_parts(&weak, &mut state, &snapshot);
            },
        );
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_playlist_rename_input_changed(move |name| {
            let mut state = state.borrow_mut();
            state.playlist_rename_input = name.to_string();
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_rename_opened_playlist(move |playlist_id, name| {
            let mut state = state.borrow_mut();
            if state.opened_playlist != Some(playlist_id) {
                state.feedback = "That playlist is no longer open.".into();
            } else {
                match state.playlists.rename(playlist_id, name.as_str()) {
                    Ok(()) => {
                        state.playlist_rename_input = state
                            .playlists
                            .playlists()
                            .iter()
                            .find(|playlist| playlist.id == playlist_id)
                            .map(|playlist| playlist.name.clone())
                            .unwrap_or_default();
                        state.feedback =
                            format!("Renamed playlist to {}.", state.playlist_rename_input);
                        persist_library(&mut state);
                    }
                    Err(error) => state.feedback = error.to_string(),
                }
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_delete_playlist(move |playlist_id| {
            let mut state = state.borrow_mut();
            if state.opened_playlist != Some(playlist_id) {
                state.feedback = "That playlist is no longer open.".into();
            } else if state.playlists.remove(playlist_id) {
                state.opened_playlist = None;
                state.playlist_rename_input.clear();
                state.page_index = 0;
                state
                    .playlist_selection
                    .retain(|(id, _)| *id != playlist_id);
                if state.playlist_target == Some(playlist_id) {
                    state.playlist_target = None;
                }
                if state
                    .active_playlist_owner
                    .as_ref()
                    .is_some_and(|owner| owner.playlist_id == playlist_id)
                {
                    state.active_playlist_owner = None;
                }
                state.feedback = "Playlist deleted. Its media files were left untouched.".into();
                persist_library(&mut state);
            } else {
                state.feedback = "That playlist no longer exists.".into();
            }
            if let Some(ui) = weak.upgrade() {
                ui.set_confirm_delete_playlist(false);
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_move_playlist_entry(
            move |playlist_id, display_index, position, track_id, requested| {
                let mut state = state.borrow_mut();
                let result = if !matches!(requested, -1 | 1) {
                    Err("Invalid playlist move direction.".to_owned())
                } else if !playlist_row_is_current(
                    &state,
                    playlist_id,
                    display_index,
                    position,
                    track_id,
                ) {
                    Err("That playlist row is stale. Refresh and try again.".to_owned())
                } else {
                    let visible_positions =
                        playlist_filtered_positions(&state, playlist_id, &state.query);
                    playlist_move_target(
                        &visible_positions,
                        usize::try_from(display_index).unwrap_or(usize::MAX),
                        requested,
                    )
                    .ok_or_else(|| {
                        "There is no displayed playlist entry in that direction.".to_owned()
                    })
                    .and_then(|target| {
                        state
                            .playlists
                            .move_entry(playlist_id, position as usize, target)
                            .then_some(target)
                            .ok_or_else(|| {
                                "That playlist entry is stale. Refresh and try again.".to_owned()
                            })
                    })
                };
                match result {
                    Ok(_) => {
                        state
                            .playlist_selection
                            .retain(|(id, _)| *id != playlist_id);
                        if state
                            .active_playlist_owner
                            .as_ref()
                            .is_some_and(|owner| owner.playlist_id == playlist_id)
                        {
                            state.active_playlist_owner = None;
                        }
                        state.feedback = "Playlist order updated.".into();
                        persist_library(&mut state);
                    }
                    Err(error) => state.feedback = error,
                }
                refresh_from_parts(&weak, &mut state, &snapshot);
            },
        );
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_remove_playlist_entry(move |playlist_id, display_index, position, track_id| {
            let mut state = state.borrow_mut();
            let result = if playlist_row_is_current(
                &state,
                playlist_id,
                display_index,
                position,
                track_id,
            ) {
                state.playlists.remove_entry(playlist_id, position as usize)
            } else {
                false
            };
            if result {
                state
                    .playlist_selection
                    .retain(|(id, _)| *id != playlist_id);
                if state
                    .active_playlist_owner
                    .as_ref()
                    .is_some_and(|owner| owner.playlist_id == playlist_id)
                {
                    state.active_playlist_owner = None;
                }
                state.feedback = "Track occurrence removed from the playlist.".into();
                persist_library(&mut state);
            } else {
                state.feedback = "That playlist row is stale. Refresh and try again.".into();
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let sender = sender.clone();
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_play_track(move |id, index| {
            let mut state = state.borrow_mut();
            let queue = music_collection(&state);
            let start = collection_start_index(&queue, id, index);
            if let Some(start) = start {
                play_collection_and_enter_video(
                    &weak, &mut state, &sender, &snapshot, queue, start, None,
                );
                state.artwork_target = Some(id);
            } else {
                state.feedback = "That track is no longer in the displayed collection.".into();
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let sender = sender.clone();
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_play_next_track(move |id, index| {
            let mut state = state.borrow_mut();
            let collection = music_collection(&state);
            if let Some(track) = collection_track_at(&collection, id, index).cloned() {
                match send_command(
                    &sender,
                    PlaybackCommand::PlayNext {
                        track: track.clone(),
                    },
                    &snapshot,
                ) {
                    Ok(()) => {
                        state.active_playlist_owner = None;
                        state.feedback = format!("Queued {} to play next.", track.title);
                    }
                    Err(error) => state.feedback = error,
                }
            } else {
                state.feedback = "That track is no longer in the displayed collection.".into();
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let sender = sender.clone();
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_play_selected(move || {
            let mut state = state.borrow_mut();
            let queue = selected_displayed_tracks(&state);
            if queue.is_empty() {
                state.feedback = "Check one or more rows on this page first.".into();
            } else {
                play_collection_and_enter_video(
                    &weak, &mut state, &sender, &snapshot, queue, 0, None,
                );
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let sender = sender.clone();
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_play_all_collection(move || {
            let mut state = state.borrow_mut();
            let queue = music_collection(&state);
            if queue.is_empty() {
                state.feedback = "There are no tracks in this collection.".into();
            } else {
                play_collection_and_enter_video(
                    &weak, &mut state, &sender, &snapshot, queue, 0, None,
                );
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }

    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_search_changed(move |query| {
            let mut state = state.borrow_mut();
            state.pending_movie_details = None;
            state.query = query.to_string();
            state.page_index = 0;
            if state.current_view == "movies" {
                state.selected_movie = None;
                state.movie_details_open = false;
            }
            state.feedback.clear();
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_previous_page(move || {
            let mut state = state.borrow_mut();
            state.page_index = state.page_index.saturating_sub(1);
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_next_page(move || {
            let mut state = state.borrow_mut();
            state.page_index = state.page_index.saturating_add(1);
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }

    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_open_audio_files(move || {
            if let Some(paths) = FileDialog::new().add_filter("Audio files", AUDIO_EXTENSIONS).pick_files() {
                let mut state = state.borrow_mut();
                let (added, metadata_available) = add_paths(&mut state, paths, Some(false));
                state.current_view = "music".into(); state.music_section = "tracks".into(); state.page_index = 0;
                if metadata_available {
                    state.feedback = format!("Added {added} audio file(s). Local tags and nearby art are being read in the background.");
                }
                persist_library(&mut state); refresh_from_parts(&weak, &mut state, &snapshot);
            }
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_open_movie_files(move || {
            if let Some(paths) = FileDialog::new().add_filter("Movie files", MOVIE_EXTENSIONS).pick_files() {
                let mut state = state.borrow_mut();
                let (added, metadata_available) = add_paths(&mut state, paths, Some(true));
                state.current_view = "movies".into(); state.movie_level = "catalog".into(); state.page_index = 0;
                if metadata_available {
                    state.feedback = format!("Added {added} movie file(s). Local poster files are being checked in the background.");
                }
                persist_library(&mut state); refresh_from_parts(&weak, &mut state, &snapshot);
            }
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_import_folder(move || {
            if let Some(root) = FileDialog::new().pick_folder() {
                let mut state = state.borrow_mut();
                let feedback = match add_watched_root(&mut state, root) {
                    Ok(true) => "Folder added to the library and scan queued.".into(),
                    Ok(false) => "Folder is already watched; a scan is queued.".into(),
                    Err(error) => error,
                };
                state.feedback = feedback;
                refresh_from_parts(&weak, &mut state, &snapshot);
            }
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_add_watched_root(move || {
            if let Some(root) = FileDialog::new().pick_folder() {
                let mut state = state.borrow_mut();
                let feedback = match add_watched_root(&mut state, root) {
                    Ok(true) => "Folder added to the library and scan queued.".into(),
                    Ok(false) => "Folder is already watched; a scan is queued.".into(),
                    Err(error) => error,
                };
                state.feedback = feedback;
                refresh_from_parts(&weak, &mut state, &snapshot);
            }
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_rescan_watched_roots(move || {
            let mut state = state.borrow_mut();
            let roots = state.watched_roots.clone();
            for root in roots {
                queue_root_scan(&mut state, root);
            }
            state.feedback = "Rescan queued for all watched folders.".into();
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_remove_watched_root(move |index| {
            let mut state = state.borrow_mut();
            if let Ok(index) = usize::try_from(index) {
                if remove_watched_root(&mut state, index).is_some() {
                    state.feedback =
                        "Folder removed from watching. Imported media and history remain.".into();
                }
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_movie_filter_changed(move |filter| {
            let mut state = state.borrow_mut();
            state.pending_movie_details = None;
            state.movie_filter = filter.to_string();
            state.selected_movie = None;
            state.movie_details_open = false;
            state.page_index = 0;
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_movie_view_changed(move |view| {
            let mut state = state.borrow_mut();
            state.pending_movie_details = None;
            state.movie_view = if view.as_str() == "my-list" {
                "my-list"
            } else {
                "all"
            }
            .into();
            state.movie_filter = "all".into();
            state.selected_movie = None;
            state.movie_details_open = false;
            state.page_index = 0;
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_movie_genre_changed(move |genre| {
            let mut state = state.borrow_mut();
            state.pending_movie_details = None;
            state.movie_genre = genre.to_string();
            state.selected_movie = None;
            state.movie_details_open = false;
            state.page_index = 0;
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_movie_sort_changed(move |sort| {
            let mut state = state.borrow_mut();
            state.pending_movie_details = None;
            state.movie_sort = movie_sort_key(sort.as_str()).into();
            state.page_index = 0;
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_music_genre_changed(move |genre| {
            let mut state = state.borrow_mut();
            state.music_genre = genre.to_string();
            state.page_index = 0;
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_music_sort_changed(move |sort| {
            let mut state = state.borrow_mut();
            state.music_sort = music_sort_key(sort.as_str()).into();
            state.music_sort_explicit = true;
            state.page_index = 0;
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_toggle_saved_movie(move |id| {
            let mut state = state.borrow_mut();
            if state.movies.iter().any(|movie| movie.id == id) {
                let removed = state.saved_movies.remove(&id);
                if !removed {
                    state.saved_movies.insert(id);
                }
                if removed && state.movie_view == "my-list" && state.selected_movie == Some(id) {
                    state.pending_movie_details = None;
                    state.movie_details_open = false;
                    state.selected_movie = None;
                }
                persist_library(&mut state);
            } else {
                state.feedback = "That movie is no longer available.".into();
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_close_movie_details(move || {
            let mut state = state.borrow_mut();
            state.pending_movie_details = None;
            state.movie_details_open = false;
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_clear_movie_filters(move || {
            let mut state = state.borrow_mut();
            state.pending_movie_details = None;
            state.query.clear();
            state.movie_view = "all".into();
            state.movie_filter = "all".into();
            state.movie_genre = "All genres".into();
            state.movie_sort = "recent".into();
            state.selected_movie = None;
            state.movie_details_open = false;
            state.page_index = 0;
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_clear_music_filters(move || {
            let mut state = state.borrow_mut();
            state.query.clear();
            state.music_genre = "All genres".into();
            state.music_sort = "recent".into();
            state.music_sort_explicit = false;
            state.page_index = 0;
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_back_movie(move || {
            let mut state = state.borrow_mut();
            if state.movie_level == "episodes" {
                state.movie_level = "seasons".into();
                state.selected_season = None;
            } else {
                state.movie_level = "catalog".into();
                state.selected_series = None;
                state.selected_season = None;
            }
            state.page_index = 0;
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_open_movie_group(move |id| {
            let mut state = state.borrow_mut();
            match state.movie_group_targets.get(&id).cloned() {
                Some(MovieGroupTarget::Series(series)) => {
                    state.selected_series = Some(series);
                    state.selected_season = None;
                    state.movie_level = "seasons".into();
                    set_movie_artwork_target(&mut state, id);
                }
                Some(MovieGroupTarget::Season(series, season)) => {
                    state.selected_series = Some(series);
                    state.selected_season = Some(season);
                    state.movie_level = "episodes".into();
                    set_movie_artwork_target(&mut state, id);
                }
                None => {
                    state.artwork_target = None;
                    state.feedback = "That series or season is no longer available.".into();
                }
            }
            state.page_index = 0;
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    let movie_details_timer = Rc::new(Timer::default());
    {
        let state_for_click = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        let timer = Rc::clone(&movie_details_timer);
        ui.on_movie_poster_clicked(move |id| {
            let mut state = state_for_click.borrow_mut();
            let clicked_at = Instant::now();
            if state.current_view == "movies"
                && movie_double_click(state.pending_movie_details, id, clicked_at)
                && movie_collection(&state).iter().any(|movie| movie.id == id)
            {
                let resume = can_resume(state.progress.get(&id));
                state.pending_movie_details = None;
                refresh_from_parts(&weak, &mut state, &snapshot);
                drop(state);
                if let Some(ui) = weak.upgrade() {
                    ui.invoke_play_movie(id, resume);
                }
                return;
            }
            if set_movie_artwork_target(&mut state, id)
                && movie_collection(&state).iter().any(|movie| movie.id == id)
            {
                state.pending_movie_details = Some((id, clicked_at));
            } else {
                state.pending_movie_details = None;
                state.feedback = "That movie is no longer available in this collection.".into();
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
            drop(state);
            let state = Rc::clone(&state_for_click);
            let weak = weak.clone();
            let snapshot = Arc::clone(&snapshot);
            timer.start(
                TimerMode::SingleShot,
                MOVIE_DETAILS_CLICK_DELAY,
                move || {
                    if weak.upgrade().is_some() {
                        let mut state = state.borrow_mut();
                        process_pending_movie_details(&mut state, Instant::now());
                        refresh_from_parts(&weak, &mut state, &snapshot);
                    }
                },
            );
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_select_movie(move |id| {
            let mut state = state.borrow_mut();
            state.pending_movie_details = None;
            if set_movie_artwork_target(&mut state, id) {
                state.selected_movie = Some(id);
                state.movie_details_open = true;
            } else {
                state.feedback = "That movie is no longer available.".into();
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let sender = sender.clone();
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_play_movie(move |id, resume| {
            let mut state = state.borrow_mut();
            state.pending_movie_details = None;
            let queue = movie_collection(&state);
            if let Some(index) = queue.iter().position(|movie| movie.id == id) {
                let position = state
                    .progress
                    .get(&id)
                    .map(|entry| entry.position_ms)
                    .unwrap_or(0);
                let resume_at = (resume && can_resume(state.progress.get(&id))).then_some(position);
                state.selected_movie = Some(id);
                state.movie_details_open = false;
                state.artwork_target = Some(id);
                play_collection_and_enter_video(
                    &weak, &mut state, &sender, &snapshot, queue, index, resume_at,
                );
                state.current_view = "movies".into();
            } else {
                state.feedback =
                    "That movie is no longer in the current filtered collection.".into();
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_exit_movie_fullscreen(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_movie_fullscreen(false);
                ui.window().set_fullscreen(false);
            }
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_return_to_video(move || {
            if !can_return_to_video(&read_snapshot(&snapshot)) {
                return;
            }
            if let Some(ui) = weak.upgrade() {
                state.borrow_mut().fullscreen_activity_at = Some(Instant::now());
                ui.set_movie_fullscreen(true);
                ui.window().set_fullscreen(true);
            }
        });
    }
    {
        let state = Rc::clone(app_state);
        ui.on_fullscreen_activity(move || {
            state.borrow_mut().fullscreen_activity_at = Some(Instant::now());
        });
    }

    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_retry_local_artwork(move || {
            let mut state = state.borrow_mut();
            if let Some(id) = artwork_target(&state, &read_snapshot(&snapshot)) {
                next_artwork_intent(&mut state, id);
                if let Some(details) = state.details.get_mut(&id) {
                    details.artwork_override = false;
                    details.artwork = None;
                }
                if track_path(&state, id).is_some() {
                    if queue_automatic_artwork_retry(&mut state, id) {
                        state.automatic_artwork_readiness.remove(&id);
                    }
                    if !state.pending_metadata.contains(&id) {
                        state.pending_metadata.push_back(id);
                    }
                    if flush_pending_metadata(&mut state) {
                        state.feedback = "Checking local tags and nearby artwork…".into();
                    }
                }
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_find_online_artwork(move || {
            let mut state = state.borrow_mut();
            if let Some(id) = artwork_target(&state, &read_snapshot(&snapshot)) {
                if let Some(track) = find_track(&state, id) {
                    let details = state.details.get(&id).cloned().unwrap_or_default();
                    let video = crate::playback::is_movie(track);
                    let episode = video.then(|| episode_info(&track.path)).flatten();
                    let request = ArtworkRequest {
                        title: episode.as_ref().map_or_else(
                            || {
                                if video || details.album.trim().is_empty() {
                                    track.title.clone()
                                } else {
                                    details.album.clone()
                                }
                            },
                            |episode| episode.series.clone(),
                        ),
                        artist: if video { None } else { nonempty(&details.album_artist, &details.artist).map(str::to_owned) },
                        album: (!video && !details.album.trim().is_empty()).then(|| details.album.clone()),
                        kind: if !video {
                            ArtworkKind::Music
                        } else if episode.is_some() {
                            ArtworkKind::Series
                        } else {
                            ArtworkKind::Movie
                        },
                    };
                    let generation = next_artwork_intent(&mut state, id);
                    let result = state.library_worker.as_ref().ok_or_else(|| "The artwork provider worker is unavailable.".to_owned()).and_then(|worker| worker.submit(LibraryJob::Artwork(id, generation, request)));
                    state.feedback = result.err().unwrap_or_else(|| "Online lookup requested. Search terms come from local tags; native media paths stay on this device.".into());
                }
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let sender = sender.clone();
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_cycle_repeat_mode(move || {
            let current = read_snapshot(&snapshot).repeat_mode;
            let next = match current {
                RepeatMode::Off => RepeatMode::All,
                RepeatMode::All => RepeatMode::One,
                RepeatMode::One => RepeatMode::Off,
            };
            let mut state = state.borrow_mut();
            if let Err(error) = send_command(&sender, PlaybackCommand::SetRepeat(next), &snapshot) {
                state.feedback = error;
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let sender = sender.clone();
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_playback_shuffle_changed(move |enabled| {
            let mut state = state.borrow_mut();
            if let Err(error) =
                send_command(&sender, PlaybackCommand::SetShuffle(enabled), &snapshot)
            {
                state.feedback = error;
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let sender = sender.clone();
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_select_audio_track(move |media_id, track_id| {
            let mut state = state.borrow_mut();
            let playback = read_snapshot(&snapshot);
            if playback.current_id == Some(media_id) {
                if let Err(error) = send_command(
                    &sender,
                    PlaybackCommand::SelectAudioTrack {
                        media_id,
                        load_generation: playback.load_generation,
                        track_id,
                    },
                    &snapshot,
                ) {
                    state.feedback = error;
                } else {
                    state.feedback.clear();
                }
            } else {
                state.feedback =
                    "Audio selection canceled because the current media changed.".into();
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let sender = sender.clone();
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_select_subtitle_track(move |media_id, track_id| {
            let mut state = state.borrow_mut();
            let playback = read_snapshot(&snapshot);
            if playback.current_id == Some(media_id) {
                if let Err(error) = send_command(
                    &sender,
                    PlaybackCommand::SelectSubtitleTrack {
                        media_id,
                        load_generation: playback.load_generation,
                        track_id,
                    },
                    &snapshot,
                ) {
                    state.feedback = error;
                } else {
                    state.feedback.clear();
                }
            } else {
                state.feedback =
                    "Subtitle selection canceled because the current media changed.".into();
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let sender = sender.clone();
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_add_subtitle_file(move || {
            let playback = read_snapshot(&snapshot);
            let (Some(media_id), load_generation) = (playback.current_id, playback.load_generation)
            else {
                let mut state = state.borrow_mut();
                state.feedback = "Select a media item before adding subtitles.".into();
                refresh_from_parts(&weak, &mut state, &snapshot);
                return;
            };
            if let Some(path) = FileDialog::new()
                .add_filter("Subtitle files", &["ass", "srt", "ssa", "sub", "vtt"])
                .pick_file()
            {
                let mut state = state.borrow_mut();
                let current = read_snapshot(&snapshot);
                if current.current_id == Some(media_id)
                    && current.load_generation == load_generation
                {
                    if let Err(error) = send_command(
                        &sender,
                        PlaybackCommand::AddSubtitle {
                            media_id,
                            load_generation,
                            path,
                        },
                        &snapshot,
                    ) {
                        state.feedback = error;
                    } else {
                        state.feedback.clear();
                    }
                } else {
                    state.feedback =
                        "Subtitle selection canceled because the current media changed.".into();
                }
                refresh_from_parts(&weak, &mut state, &snapshot);
            }
        });
    }
    {
        let state = Rc::clone(app_state);
        ui.on_audio_delay_draft_changed(move |value| {
            state.borrow_mut().audio_delay_input = value.to_string();
        });
    }
    {
        let state = Rc::clone(app_state);
        let sender = sender.clone();
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_audio_delay_changed(move |value| {
            let mut state = state.borrow_mut();
            state.audio_delay_input = value.to_string();
            let playback = read_snapshot(&snapshot);
            if let (Some(media_id), Some(delay_us)) =
                (playback.current_id, parse_delay_us(value.as_str()))
            {
                if let Err(error) = send_command(
                    &sender,
                    PlaybackCommand::SetAudioDelay {
                        media_id,
                        load_generation: playback.load_generation,
                        delay_us,
                    },
                    &snapshot,
                ) {
                    state.feedback = error;
                } else {
                    state.feedback.clear();
                }
            } else if playback.current_id.is_none() {
                state.feedback = "Select a media item before adjusting audio delay.".into();
            } else {
                state.feedback = "Enter milliseconds from −60000 to +60000.".into();
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        ui.on_subtitle_delay_draft_changed(move |value| {
            state.borrow_mut().subtitle_delay_input = value.to_string();
        });
    }
    {
        let state = Rc::clone(app_state);
        let sender = sender.clone();
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_subtitle_delay_changed(move |value| {
            let mut state = state.borrow_mut();
            state.subtitle_delay_input = value.to_string();
            let playback = read_snapshot(&snapshot);
            if let (Some(media_id), Some(delay_us)) =
                (playback.current_id, parse_delay_us(value.as_str()))
            {
                if let Err(error) = send_command(
                    &sender,
                    PlaybackCommand::SetSubtitleDelay {
                        media_id,
                        load_generation: playback.load_generation,
                        delay_us,
                    },
                    &snapshot,
                ) {
                    state.feedback = error;
                } else {
                    state.feedback.clear();
                }
            } else if playback.current_id.is_none() {
                state.feedback = "Select a media item before adjusting subtitle delay.".into();
            } else {
                state.feedback = "Enter milliseconds from −60000 to +60000.".into();
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_jump_time_changed(move |value| {
            let mut state = state.borrow_mut();
            state.jump_time_input = value.to_string();
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let sender = sender.clone();
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_jump_to_time(move || {
            let mut state = state.borrow_mut();
            let playback = read_snapshot(&snapshot);
            if let (Some(track_id), Some(position_ms)) =
                (playback.current_id, parse_time_ms(&state.jump_time_input))
            {
                let request_id = next_control_request_id(&mut state);
                let submitted_at = Instant::now();
                if submit_transport_command(
                    &mut state,
                    &sender,
                    &snapshot,
                    PlaybackCommand::SeekTo {
                        request_id,
                        track_id,
                        load_generation: playback.load_generation,
                        position_ms,
                    },
                ) {
                    state.pending_seek = Some(PendingSeek {
                        position_ms,
                        track_id,
                        load_generation: playback.load_generation,
                        changed_at: submitted_at,
                        request_id: Some(request_id),
                        submitted_at: Some(submitted_at),
                    });
                    if let Some(ui) = weak.upgrade() {
                        ui.set_adjusting_seek(true);
                    }
                }
            } else {
                state.feedback = "Enter seconds, mm:ss or hh:mm:ss.".into();
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let sender = sender.clone();
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_select_chapter(move |media_id, chapter| {
            let mut state = state.borrow_mut();
            let playback = read_snapshot(&snapshot);
            if playback.current_id == Some(media_id) {
                if let Err(error) = send_command(
                    &sender,
                    PlaybackCommand::SelectChapter {
                        media_id,
                        load_generation: playback.load_generation,
                        chapter,
                    },
                    &snapshot,
                ) {
                    state.feedback = error;
                } else {
                    state.feedback.clear();
                }
            } else {
                state.feedback =
                    "Chapter selection canceled because the current media changed.".into();
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_set_video_fill(move |enabled| {
            let mut state = state.borrow_mut();
            state.video_fill = enabled;
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }

    for operation in [
        QueueOperation::Jump,
        QueueOperation::Move,
        QueueOperation::Remove,
    ] {
        let state = Rc::clone(app_state);
        let sender = sender.clone();
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        match operation {
            QueueOperation::Jump => ui.on_queue_jump(move |revision, position| {
                queue_command(
                    &state,
                    &sender,
                    &snapshot,
                    &weak,
                    revision.as_str(),
                    position,
                    None,
                    QueueOperation::Jump,
                )
            }),
            QueueOperation::Move => ui.on_queue_move(move |revision, from, to| {
                queue_command(
                    &state,
                    &sender,
                    &snapshot,
                    &weak,
                    revision.as_str(),
                    from,
                    Some(to),
                    QueueOperation::Move,
                )
            }),
            QueueOperation::Remove => ui.on_queue_remove(move |revision, position| {
                queue_command(
                    &state,
                    &sender,
                    &snapshot,
                    &weak,
                    revision.as_str(),
                    position,
                    None,
                    QueueOperation::Remove,
                )
            }),
        }
    }
}

#[derive(Clone, Copy)]
enum QueueOperation {
    Jump,
    Move,
    Remove,
}

const AUDIO_EXTENSIONS: &[&str] = &[
    "aac", "aif", "aiff", "alac", "ape", "flac", "m4a", "m4b", "mid", "midi", "mp3", "mpc", "oga",
    "ogg", "opus", "wav", "wma", "wv",
];
const MOVIE_EXTENSIONS: &[&str] = &[
    "3g2", "3gp", "asf", "avi", "divx", "dv", "flv", "m2ts", "m2v", "m4v", "mkv", "mov", "mp2",
    "mp4", "mpe", "mpeg", "mpg", "mts", "mxf", "ogv", "qt", "rm", "rmvb", "ts", "vob", "webm",
    "wmv",
];

fn refresh_from_state(
    weak: &slint::Weak<AppWindow>,
    state: &Rc<RefCell<AppState>>,
    snapshot: &Arc<Mutex<PlaybackSnapshot>>,
) {
    if let Some(ui) = weak.upgrade() {
        let mut state = state.borrow_mut();
        let playback = read_snapshot(snapshot);
        refresh_ui(&ui, &mut state, &playback);
    }
}

fn refresh_from_parts(
    weak: &slint::Weak<AppWindow>,
    state: &mut AppState,
    snapshot: &Arc<Mutex<PlaybackSnapshot>>,
) {
    if let Some(ui) = weak.upgrade() {
        let playback = read_snapshot(snapshot);
        refresh_ui(&ui, state, &playback);
    }
}

fn find_track(state: &AppState, id: TrackId) -> Option<&Track> {
    state
        .tracks
        .iter()
        .chain(&state.movies)
        .find(|track| track.id == id)
}

fn track_path(state: &AppState, id: TrackId) -> Option<PathBuf> {
    find_track(state, id)
        .filter(|track| track.source == TrackSource::Local)
        .map(|track| track.path.clone())
}

fn add_paths(
    state: &mut AppState,
    paths: Vec<PathBuf>,
    force_video: Option<bool>,
) -> (usize, bool) {
    let mut added = 0;
    let mut metadata_ids = Vec::new();
    let mut metadata_available = true;
    let mut known_paths = state
        .tracks
        .iter()
        .chain(&state.movies)
        .filter(|track| track.source == TrackSource::Local)
        .map(|track| path_identity_key(&track.path))
        .collect::<HashSet<_>>();
    for selected in paths {
        let requested_identity = path_identity_key(&selected);
        if known_paths.contains(&requested_identity) {
            continue;
        }
        let path = std::fs::canonicalize(&selected).unwrap_or(selected);
        let identity = path_identity_key(&path);
        if known_paths.contains(&identity) {
            continue;
        }
        let video = force_video.unwrap_or_else(|| is_movie_path(&path));
        let id = state.next_id;
        if id <= 0 || id == TrackId::MAX {
            state.feedback = "This library has reached its media ID limit.".into();
            metadata_available = false;
            break;
        }
        let track = if video {
            movie_from_path(id, path.clone())
        } else {
            Track::from_path(id, path.clone())
        };
        if let Some(track) = track {
            if video {
                state.movies.push(track);
            } else {
                state.tracks.push(track);
            }
            known_paths.insert(requested_identity);
            known_paths.insert(identity);
            metadata_ids.push(id);
            state.next_id = id + 1;
            added += 1;
        }
    }
    if !metadata_ids.is_empty() {
        state.pending_metadata.extend(metadata_ids);
        metadata_available &= flush_pending_metadata(state);
        persist_library(state);
    }
    (added, metadata_available)
}

fn flush_pending_metadata(state: &mut AppState) -> bool {
    loop {
        let ids = state
            .pending_metadata
            .iter()
            .take(METADATA_BATCH_SIZE)
            .copied()
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return true;
        }

        let paths = ids
            .iter()
            .filter_map(|id| track_path(state, *id).map(|path| (*id, path)))
            .collect::<Vec<_>>();
        if paths.is_empty() {
            state.pending_metadata.drain(..ids.len());
            continue;
        }

        let result = state
            .library_worker
            .as_ref()
            .ok_or(crate::library_worker::QueueSubmitError::Stopped)
            .and_then(|worker| worker.try_submit(LibraryJob::ReadMetadata(paths)));
        match result {
            Ok(()) => {
                state.pending_metadata.drain(..ids.len());
            }
            Err(crate::library_worker::QueueSubmitError::Full) => return true,
            Err(crate::library_worker::QueueSubmitError::Stopped) => {
                state.feedback =
                    "Metadata reading is unavailable; the media entries remain in memory.".into();
                return false;
            }
        }
    }
}

fn wire_picture_in_picture(
    ui: &AppWindow,
    app_state: &Rc<RefCell<AppState>>,
    snapshot: &Arc<Mutex<PlaybackSnapshot>>,
    picture_in_picture: &Rc<RefCell<Option<PictureInPictureWindow>>>,
    displayed_frame: &Rc<RefCell<Option<DisplayedVideoFrame>>>,
) {
    {
        let app_state = Rc::clone(app_state);
        let picture_in_picture = Rc::clone(picture_in_picture);
        let displayed_frame = Rc::clone(displayed_frame);
        let snapshot = Arc::clone(snapshot);
        let weak_main = ui.as_weak();
        let return_to_fullscreen = Rc::new(Cell::new(false));
        let return_to_fullscreen_open = Rc::clone(&return_to_fullscreen);
        ui.on_open_picture_in_picture(move || {
            let playback = read_snapshot(&snapshot);
            let Some(_) = displayed_frame.borrow().as_ref() else {
                let mut state = app_state.borrow_mut();
                state.feedback =
                    "Picture in picture is available after a video frame appears.".into();
                refresh_from_parts(&weak_main, &mut state, &snapshot);
                return;
            };
            if !playback.current_is_video || playback.current_id.is_none() {
                let mut state = app_state.borrow_mut();
                state.feedback = "Choose a video before opening picture in picture.".into();
                refresh_from_parts(&weak_main, &mut state, &snapshot);
                return;
            }
            let Some(main) = weak_main.upgrade() else {
                return;
            };
            return_to_fullscreen_open.set(main.get_movie_fullscreen());

            let mut picture_in_picture = picture_in_picture.borrow_mut();
            if picture_in_picture.is_none() {
                let pip = match PictureInPictureWindow::new() {
                    Ok(pip) => pip,
                    Err(error) => {
                        let mut state = app_state.borrow_mut();
                        state.feedback = format!("Picture in picture could not open: {error}");
                        refresh_from_parts(&weak_main, &mut state, &snapshot);
                        return;
                    }
                };
                let pip_weak_for_close = pip.as_weak();
                let main_weak_for_close = weak_main.clone();
                let main_weak_for_toggle = weak_main.clone();
                pip.on_toggle_pause(move || {
                    if let Some(main) = main_weak_for_toggle.upgrade() {
                        main.invoke_toggle_pause();
                    }
                });
                pip.on_close_picture_in_picture(move || {
                    if let Some(pip) = pip_weak_for_close.upgrade() {
                        let _ = pip.hide();
                    }
                    if let Some(main) = main_weak_for_close.upgrade() {
                        main.set_picture_in_picture_open(false);
                    }
                });

                let pip_weak_for_return = pip.as_weak();
                let main_weak_for_return = weak_main.clone();
                let return_to_fullscreen = Rc::clone(&return_to_fullscreen);
                pip.on_return_to_player(move || {
                    if let Some(pip) = pip_weak_for_return.upgrade() {
                        let _ = pip.hide();
                    }
                    if let Some(main) = main_weak_for_return.upgrade() {
                        main.set_picture_in_picture_open(false);
                        main.window().set_minimized(false);
                        let _ = main.show();
                        if return_to_fullscreen.get() {
                            main.invoke_return_to_video();
                        } else {
                            main.set_movie_fullscreen(false);
                            main.window().set_fullscreen(false);
                        }
                    }
                });

                let main_weak_for_native_close = weak_main.clone();
                pip.window().on_close_requested(move || {
                    if let Some(main) = main_weak_for_native_close.upgrade() {
                        main.set_picture_in_picture_open(false);
                    }
                    CloseRequestResponse::HideWindow
                });
                *picture_in_picture = Some(pip);
            }

            let Some(pip) = picture_in_picture.as_ref() else {
                return;
            };
            pip.set_video_frame(main.get_video_frame());
            pip.set_has_video_frame(main.get_has_video_frame());
            pip.set_current_title(SharedString::from(playback.current_title.as_str()));
            pip.set_player_state(SharedString::from(playback.state.label()));
            pip.set_can_toggle_pause(can_toggle_pause(&playback));
            pip.set_is_paused(playback.state == PlaybackUiState::Paused);
            match pip.show() {
                Ok(()) => {
                    main.set_picture_in_picture_open(true);
                    let mut state = app_state.borrow_mut();
                    state.feedback.clear();
                    refresh_from_parts(&weak_main, &mut state, &snapshot);
                }
                Err(error) => {
                    let mut state = app_state.borrow_mut();
                    state.feedback = format!("Picture in picture could not open: {error}");
                    refresh_from_parts(&weak_main, &mut state, &snapshot);
                }
            }
        });
    }
    {
        let picture_in_picture = Rc::clone(picture_in_picture);
        let weak_main = ui.as_weak();
        ui.on_close_picture_in_picture(move || {
            if let Some(pip) = picture_in_picture.borrow().as_ref() {
                let _ = pip.hide();
            }
            if let Some(main) = weak_main.upgrade() {
                main.set_picture_in_picture_open(false);
            }
        });
    }
    {
        let app_state = Rc::clone(app_state);
        let displayed_frame = Rc::clone(displayed_frame);
        let weak_main = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_save_video_frame(move || {
            let Some(frame) = displayed_frame.borrow().clone() else {
                let mut state = app_state.borrow_mut();
                state.feedback = "A screenshot is available after a video frame appears.".into();
                refresh_from_parts(&weak_main, &mut state, &snapshot);
                return;
            };
            let Some(path) = FileDialog::new()
                .add_filter("PNG image", &["png"])
                .set_file_name("screenshot.png")
                .save_file()
            else {
                return;
            };
            let result = image::save_buffer_with_format(
                &path,
                &frame.rgba,
                frame.width,
                frame.height,
                image::ColorType::Rgba8,
                image::ImageFormat::Png,
            );
            let mut state = app_state.borrow_mut();
            state.feedback = match result {
                Ok(()) => format!("Saved current video frame to {}.", path.display()),
                Err(error) => format!("Unable to save video screenshot: {error}"),
            };
            refresh_from_parts(&weak_main, &mut state, &snapshot);
        });
    }
}

fn wire_extended_tools(
    ui: &AppWindow,
    app_state: &Rc<RefCell<AppState>>,
    sender: &SyncSender<PlaybackCommand>,
    snapshot: &Arc<Mutex<PlaybackSnapshot>>,
) {
    {
        let state = Rc::clone(app_state);
        let sender = sender.clone();
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_equalizer_enabled_changed(move |enabled| {
            let mut state = state.borrow_mut();
            match send_command(
                &sender,
                PlaybackCommand::SetEqualizerEnabled { enabled },
                &snapshot,
            ) {
                Ok(()) => {
                    state.equalizer_settings.enabled = enabled;
                    state.equalizer_settings.preset_index = None;
                    state.equalizer_settings_pending = true;
                    state.equalizer_pending_preset = None;
                    state.equalizer_restore_failed = false;
                    state.equalizer_restore_fallback = false;
                    state.equalizer_restore_sent_at = None;
                    persist_library(&mut state);
                    state.feedback.clear();
                }
                Err(error) => state.feedback = error,
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let sender = sender.clone();
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_equalizer_preset_changed(move |preset| {
            let mut state = state.borrow_mut();
            let playback = read_snapshot(&snapshot);
            let Some(index) = playback
                .equalizer_presets
                .iter()
                .position(|label| label == preset.as_str())
            else {
                state.feedback = "That equalizer preset is no longer available.".into();
                refresh_from_parts(&weak, &mut state, &snapshot);
                return;
            };
            match send_command(
                &sender,
                PlaybackCommand::SetEqualizerPreset { index },
                &snapshot,
            ) {
                Ok(()) => {
                    state.equalizer_settings.preset_index = Some(index);
                    state.equalizer_settings_pending = true;
                    state.equalizer_pending_preset = Some(index);
                    state.equalizer_restore_failed = false;
                    state.equalizer_restore_fallback = false;
                    state.equalizer_restore_sent_at = None;
                    state.equalizer_editing = false;
                    persist_library(&mut state);
                    state.feedback.clear();
                }
                Err(error) => state.feedback = error,
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        ui.on_equalizer_preamp_draft_changed(move |gain| {
            let mut state = state.borrow_mut();
            if gain.is_finite() {
                state.equalizer_settings.preamp_db = gain.clamp(
                    crate::playback::MIN_EQUALIZER_GAIN_DB,
                    crate::playback::MAX_EQUALIZER_GAIN_DB,
                );
                state.equalizer_settings.preset_index = None;
                state.equalizer_editing = true;
            }
        });
    }
    {
        let state = Rc::clone(app_state);
        let sender = sender.clone();
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_equalizer_preamp_changed(move |gain| {
            let mut state = state.borrow_mut();
            let gain = gain.clamp(
                crate::playback::MIN_EQUALIZER_GAIN_DB,
                crate::playback::MAX_EQUALIZER_GAIN_DB,
            );
            match send_command(
                &sender,
                PlaybackCommand::SetEqualizerPreamp { gain_db: gain },
                &snapshot,
            ) {
                Ok(()) => {
                    state.equalizer_settings.preamp_db = gain;
                    state.equalizer_settings.preset_index = None;
                    state.equalizer_settings_pending = true;
                    state.equalizer_pending_preset = None;
                    state.equalizer_restore_failed = false;
                    state.equalizer_restore_fallback = false;
                    state.equalizer_restore_sent_at = None;
                    persist_library(&mut state);
                    state.feedback.clear();
                }
                Err(error) => {
                    state.equalizer_settings = read_snapshot(&snapshot).equalizer_settings;
                    state.equalizer_settings_pending = false;
                    state.feedback = error;
                }
            }
            state.equalizer_editing = false;
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let snapshot = Arc::clone(snapshot);
        ui.on_equalizer_preamp_cancelled(move || {
            let mut state = state.borrow_mut();
            state.equalizer_settings = read_snapshot(&snapshot).equalizer_settings;
            state.equalizer_editing = false;
        });
    }
    {
        let state = Rc::clone(app_state);
        ui.on_equalizer_band_draft_changed(move |index, gain| {
            let mut state = state.borrow_mut();
            if let (Ok(index), true) = (usize::try_from(index), gain.is_finite()) {
                if let Some(value) = state.equalizer_settings.bands_db.get_mut(index) {
                    *value = gain.clamp(
                        crate::playback::MIN_EQUALIZER_GAIN_DB,
                        crate::playback::MAX_EQUALIZER_GAIN_DB,
                    );
                    state.equalizer_settings.preset_index = None;
                    state.equalizer_editing = true;
                }
            }
        });
    }
    {
        let state = Rc::clone(app_state);
        let sender = sender.clone();
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_equalizer_band_changed(move |index, gain| {
            let mut state = state.borrow_mut();
            let result = usize::try_from(index)
                .ok()
                .filter(|index| *index < state.equalizer_settings.bands_db.len())
                .filter(|_| gain.is_finite())
                .ok_or_else(|| "The equalizer band selection is invalid.".to_owned())
                .and_then(|index| {
                    let gain = gain.clamp(
                        crate::playback::MIN_EQUALIZER_GAIN_DB,
                        crate::playback::MAX_EQUALIZER_GAIN_DB,
                    );
                    send_command(
                        &sender,
                        PlaybackCommand::SetEqualizerBand {
                            index,
                            gain_db: gain,
                        },
                        &snapshot,
                    )
                    .map(|()| (index, gain))
                });
            match result {
                Ok((index, gain)) => {
                    state.equalizer_settings.bands_db[index] = gain;
                    state.equalizer_settings.preset_index = None;
                    state.equalizer_settings_pending = true;
                    state.equalizer_pending_preset = None;
                    state.equalizer_restore_failed = false;
                    state.equalizer_restore_fallback = false;
                    state.equalizer_restore_sent_at = None;
                    persist_library(&mut state);
                    state.feedback.clear();
                }
                Err(error) => {
                    state.equalizer_settings = read_snapshot(&snapshot).equalizer_settings;
                    state.equalizer_settings_pending = false;
                    state.feedback = error;
                }
            }
            state.equalizer_editing = false;
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let snapshot = Arc::clone(snapshot);
        ui.on_equalizer_band_cancelled(move || {
            let mut state = state.borrow_mut();
            state.equalizer_settings = read_snapshot(&snapshot).equalizer_settings;
            state.equalizer_editing = false;
        });
    }
    {
        let state = Rc::clone(app_state);
        let sender = sender.clone();
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_set_ab_repeat_point(move |point, track_id, load_generation| {
            let mut state = state.borrow_mut();
            let playback = read_snapshot(&snapshot);
            let result = parse_control_identity(track_id, load_generation.as_str(), &playback)
                .ok_or_else(|| "The current media changed before the A/B point was set.".to_owned())
                .and_then(|(track_id, load_generation)| {
                    let point = match point.as_str() {
                        "A" => AbPoint::A,
                        "B" => AbPoint::B,
                        _ => return Err("Choose point A or B.".into()),
                    };
                    send_command(
                        &sender,
                        PlaybackCommand::SetAbRepeatPoint {
                            point,
                            track_id,
                            load_generation,
                        },
                        &snapshot,
                    )
                });
            state.feedback = result.err().unwrap_or_default();
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let sender = sender.clone();
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_clear_ab_repeat(move |track_id, load_generation| {
            let mut state = state.borrow_mut();
            let playback = read_snapshot(&snapshot);
            let result = parse_control_identity(track_id, load_generation.as_str(), &playback)
                .ok_or_else(|| "The current media changed before the loop was cleared.".to_owned())
                .and_then(|(track_id, load_generation)| {
                    send_command(
                        &sender,
                        PlaybackCommand::ClearAbRepeat {
                            track_id,
                            load_generation,
                        },
                        &snapshot,
                    )
                });
            state.feedback = result.err().unwrap_or_default();
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let sender = sender.clone();
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_set_sleep_timer(move |minutes| {
            let mut state = state.borrow_mut();
            let duration_ms = match minutes {
                0 => Some(None),
                15 | 30 | 60 => Some(Some(minutes as u64 * 60_000)),
                _ => None,
            };
            let result = duration_ms
                .ok_or_else(|| "Choose 15, 30 or 60 minutes, or cancel the timer.".to_owned())
                .and_then(|duration_ms| {
                    send_command(
                        &sender,
                        PlaybackCommand::SetSleepTimer { duration_ms },
                        &snapshot,
                    )
                });
            state.feedback = result.err().unwrap_or_default();
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        ui.on_cast_address_draft_changed(move |address| {
            state.borrow_mut().cast_address_input = address.to_string();
        });
    }
    {
        let state = Rc::clone(app_state);
        let sender = sender.clone();
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_set_cast_target(move |address| {
            let mut state = state.borrow_mut();
            let address = address.trim();
            if address.is_empty() {
                state.feedback = "Enter a cast device IP address or hostname.".into();
            } else if let Err(error) = send_command(
                &sender,
                PlaybackCommand::SetCastTarget {
                    address: Some(address.to_owned()),
                },
                &snapshot,
            ) {
                state.feedback = error;
            } else {
                state.feedback.clear();
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let sender = sender.clone();
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_use_local_output(move || {
            let mut state = state.borrow_mut();
            if let Err(error) = send_command(
                &sender,
                PlaybackCommand::SetCastTarget { address: None },
                &snapshot,
            ) {
                state.feedback = error;
            } else {
                state.feedback.clear();
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        ui.on_subtitle_languages_changed(move |languages| {
            state.borrow_mut().subtitle_language_input = languages.to_string();
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_search_subtitles(move |languages| {
            let mut state = state.borrow_mut();
            let playback = read_snapshot(&snapshot);
            let Some(media_id) = playback.current_id.filter(|_| playback.current_is_video) else {
                state.feedback = "Choose a video before searching for subtitles.".into();
                refresh_from_parts(&weak, &mut state, &snapshot);
                return;
            };
            let request_id = state.next_subtitle_request_id;
            let Some(next_request_id) = request_id.checked_add(1) else {
                state.feedback = "The subtitle request limit has been reached. Restart the application to continue.".into();
                refresh_from_parts(&weak, &mut state, &snapshot);
                return;
            };
            let query = find_track(&state, media_id)
                .map(|track| track.title.trim().to_owned())
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| playback.current_title.clone());
            if query.trim().is_empty() {
                state.feedback = "This video has no title to search for.".into();
                refresh_from_parts(&weak, &mut state, &snapshot);
                return;
            }
            let languages = languages
                .split(',')
                .map(str::trim)
                .filter(|language| !language.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let job = LibraryJob::SearchSubtitles {
                request_id,
                query: query.clone(),
                languages,
            };
            let submitted = state
                .library_worker
                .as_ref()
                .ok_or(crate::library_worker::QueueSubmitError::Stopped)
                .and_then(|worker| worker.try_submit(job));
            match submitted {
                Ok(()) => {
                    state.next_subtitle_request_id = next_request_id;
                    state.subtitle_search_pending = Some(PendingSubtitleRequest {
                        request_id,
                        media_id,
                        load_generation: playback.load_generation,
                    });
                    state.subtitle_results.clear();
                    state.subtitle_search_status = format!("Searching OpenSubtitles for {query}…");
                    state.feedback.clear();
                }
                Err(error) => state.feedback = error.message().into(),
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_download_subtitle(move |index| {
            let mut state = state.borrow_mut();
            let playback = read_snapshot(&snapshot);
            let Some(media_id) = playback.current_id.filter(|_| playback.current_is_video) else {
                state.feedback = "Choose a video before downloading a subtitle.".into();
                refresh_from_parts(&weak, &mut state, &snapshot);
                return;
            };
            let Some(result) = usize::try_from(index)
                .ok()
                .and_then(|index| state.subtitle_results.get(index))
            else {
                state.feedback = "That subtitle result is no longer available.".into();
                refresh_from_parts(&weak, &mut state, &snapshot);
                return;
            };
            let file_id = result.file_id;
            let request_id = state.next_subtitle_request_id;
            let Some(next_request_id) = request_id.checked_add(1) else {
                state.feedback = "The subtitle request limit has been reached. Restart the application to continue.".into();
                refresh_from_parts(&weak, &mut state, &snapshot);
                return;
            };
            let submitted = state
                .library_worker
                .as_ref()
                .ok_or(crate::library_worker::QueueSubmitError::Stopped)
                .and_then(|worker| {
                    worker.try_submit(LibraryJob::DownloadSubtitle {
                        request_id,
                        file_id,
                    })
                });
            match submitted {
                Ok(()) => {
                    state.next_subtitle_request_id = next_request_id;
                    state.subtitle_download_pending = Some(PendingSubtitleRequest {
                        request_id,
                        media_id,
                        load_generation: playback.load_generation,
                    });
                    state.subtitle_search_status = "Downloading subtitle to the application cache…".into();
                    state.feedback.clear();
                }
                Err(error) => state.feedback = error.message().into(),
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_save_opensubtitles_credentials(move |api_key, bearer_token| {
            let mut state = state.borrow_mut();
            let api_key = api_key.trim();
            let bearer_token = bearer_token.trim();
            if api_key.is_empty() {
                state.opensubtitles_status =
                    "Enter an OpenSubtitles API key. The bearer token is optional for search."
                        .into();
            } else {
                match crate::credentials::save_opensubtitles_credentials(
                    api_key,
                    (!bearer_token.is_empty()).then_some(bearer_token),
                ) {
                    Ok(()) => {
                        state.opensubtitles_credentials_available = true;
                        state.opensubtitles_status =
                            "OpenSubtitles credentials saved in the system credential store."
                                .into();
                        if let Some(ui) = weak.upgrade() {
                            ui.set_opensubtitles_api_key_input(SharedString::default());
                            ui.set_opensubtitles_bearer_token_input(SharedString::default());
                        }
                    }
                    Err(error) => state.opensubtitles_status = error,
                }
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_remove_opensubtitles_credentials(move || {
            let mut state = state.borrow_mut();
            match crate::credentials::clear_opensubtitles_credentials() {
                Ok(()) => {
                    state.opensubtitles_credentials_available = false;
                    state.opensubtitles_status =
                        "OpenSubtitles credentials removed from the system credential store."
                            .into();
                    if let Some(ui) = weak.upgrade() {
                        ui.set_opensubtitles_api_key_input(SharedString::default());
                        ui.set_opensubtitles_bearer_token_input(SharedString::default());
                    }
                }
                Err(error) => state.opensubtitles_status = error,
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        ui.on_bookmark_name_changed(move |name| {
            state.borrow_mut().bookmark_name_input = name.to_string();
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_add_bookmark(move || {
            let mut state = state.borrow_mut();
            let playback = read_snapshot(&snapshot);
            let Some(media_id) = playback.current_id else {
                state.feedback = "Choose media before adding a bookmark.".into();
                refresh_from_parts(&weak, &mut state, &snapshot);
                return;
            };
            if state.bookmarks.len() >= 500 {
                state.feedback = "This library has reached its 500-bookmark limit.".into();
                refresh_from_parts(&weak, &mut state, &snapshot);
                return;
            }
            let id = state
                .bookmarks
                .iter()
                .map(|bookmark| bookmark.id)
                .max()
                .unwrap_or(0)
                .saturating_add(1);
            if id <= 0 {
                state.feedback = "This library has reached its bookmark ID limit.".into();
                refresh_from_parts(&weak, &mut state, &snapshot);
                return;
            }
            let name = match bookmark_name(&state.bookmark_name_input, playback.position_ms) {
                Ok(name) => name,
                Err(message) => {
                    state.feedback = message.into();
                    refresh_from_parts(&weak, &mut state, &snapshot);
                    return;
                }
            };
            state.bookmarks.push(Bookmark {
                id,
                media_id,
                name,
                position_ms: playback.position_ms.max(0),
            });
            state.bookmark_name_input.clear();
            state.feedback = "Bookmark added.".into();
            persist_library(&mut state);
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let app_state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_bookmark_jump(move |index| {
            let playback = read_snapshot(&snapshot);
            let result = {
                let state = app_state.borrow();
                usize::try_from(index)
                    .ok()
                    .and_then(|index| state.bookmarks.get(index))
                    .filter(|bookmark| Some(bookmark.media_id) == playback.current_id)
                    .cloned()
                    .ok_or_else(|| "That bookmark is not for the current media.".to_owned())
            };
            match result {
                Ok(bookmark) => {
                    if let Some(ui) = weak.upgrade() {
                        ui.invoke_seek_changed(
                            bookmark.position_ms.max(0) as f32,
                            bookmark.media_id,
                            SharedString::from(playback.load_generation.to_string()),
                            true,
                        );
                    }
                }
                Err(error) => {
                    let mut state = app_state.borrow_mut();
                    state.feedback = error;
                    refresh_from_parts(&weak, &mut state, &snapshot);
                }
            }
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_remove_bookmark(move |index| {
            let mut state = state.borrow_mut();
            if let Ok(index) = usize::try_from(index) {
                if index < state.bookmarks.len() {
                    state.bookmarks.remove(index);
                    state.feedback = "Bookmark removed.".into();
                    persist_library(&mut state);
                }
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        ui.on_network_url_input_changed(move |url| {
            state.borrow_mut().network_url_input = url.to_string();
        });
    }
    {
        let state = Rc::clone(app_state);
        let sender = sender.clone();
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_play_network_url(move |audio_only| {
            let mut state = state.borrow_mut();
            let input = state.network_url_input.trim().to_owned();
            let result = NetworkUrl::parse(&input)
                .map_err(|error| error.to_string())
                .and_then(|url| {
                    ensure_network_track(&mut state, &url, audio_only).map(|track| (track, url))
                });
            match result {
                Ok((track, _)) => {
                    if audio_only {
                        state.current_view = "music".into();
                        state.music_section = "tracks".into();
                    } else {
                        state.current_view = "movies".into();
                        state.movie_level = "catalog".into();
                    }
                    if play_collection_and_enter_video(
                        &weak,
                        &mut state,
                        &sender,
                        &snapshot,
                        vec![track],
                        0,
                        None,
                    ) {
                        state.network_url_input.clear();
                        persist_library(&mut state);
                    }
                }
                Err(error) => state.feedback = error,
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let sender = sender.clone();
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_open_network_history(move |index| {
            let mut state = state.borrow_mut();
            let entry = usize::try_from(index)
                .ok()
                .and_then(|index| state.network_history.get(index))
                .cloned();
            let result = entry
                .ok_or_else(|| "That network address is no longer in history.".to_owned())
                .and_then(|entry| {
                    let audio_only = state.tracks.iter().any(|track| {
                        matches!(&track.source, TrackSource::Network(url) if *url == entry.url)
                            && track.format == "STREAM"
                    });
                    ensure_network_track(&mut state, &entry.url, audio_only)
                        .map(|track| (track, audio_only))
                });
            match result {
                Ok((track, audio_only)) => {
                    if audio_only {
                        state.current_view = "music".into();
                        state.music_section = "tracks".into();
                    } else {
                        state.current_view = "movies".into();
                        state.movie_level = "catalog".into();
                    }
                    let _ = play_collection_and_enter_video(
                        &weak,
                        &mut state,
                        &sender,
                        &snapshot,
                        vec![track],
                        0,
                        None,
                    );
                }
                Err(error) => state.feedback = error,
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_remove_network_history(move |index| {
            let mut state = state.borrow_mut();
            if let Ok(index) = usize::try_from(index) {
                if index < state.network_history.len() {
                    state.network_history.remove(index);
                    persist_library(&mut state);
                    state.feedback = "Network history entry removed.".into();
                }
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
}

fn wire_playlist_io(
    ui: &AppWindow,
    app_state: &Rc<RefCell<AppState>>,
    snapshot: &Arc<Mutex<PlaybackSnapshot>>,
) {
    {
        let app_state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_import_m3u8(move || {
            let Some(path) = FileDialog::new()
                .add_filter("M3U8 playlist", &["m3u8", "m3u"])
                .pick_file()
            else {
                return;
            };
            let result = {
                let mut state = app_state.borrow_mut();
                import_m3u8_into_state(&mut state, &path)
            };
            let mut state = app_state.borrow_mut();
            match result {
                Ok(message) => {
                    state.playlist_io_status = message.clone();
                    state.feedback = message;
                    persist_library(&mut state);
                }
                Err(error) => {
                    state.playlist_io_status = error.clone();
                    state.feedback = error;
                }
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
    {
        let app_state = Rc::clone(app_state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(snapshot);
        ui.on_export_m3u8(move |playlist_id| {
            let source = {
                let state = app_state.borrow();
                let playlist = state
                    .playlists
                    .playlists()
                    .iter()
                    .find(|playlist| playlist.id == playlist_id)
                    .cloned();
                playlist.map(|playlist| {
                    let tracks = state
                        .tracks
                        .iter()
                        .chain(&state.movies)
                        .cloned()
                        .collect::<Vec<_>>();
                    (playlist, tracks)
                })
            };
            let Some((playlist, tracks)) = source else {
                let mut state = app_state.borrow_mut();
                state.playlist_io_status = "Choose an opened playlist to export.".into();
                refresh_from_parts(&weak, &mut state, &snapshot);
                return;
            };
            let file_name = format!("{}.m3u8", safe_filename(&playlist.name));
            let Some(path) = FileDialog::new()
                .add_filter("M3U8 playlist", &["m3u8"])
                .set_file_name(&file_name)
                .save_file()
            else {
                return;
            };
            let export = path
                .parent()
                .ok_or_else(|| "Choose a playlist file inside a folder.".to_owned())
                .and_then(|directory| {
                    crate::playlists::export_m3u8(&playlist, &tracks, directory)
                        .map_err(|error| error.to_string())
                })
                .and_then(|text| {
                    std::fs::write(&path, text.as_bytes()).map_err(|error| error.to_string())
                });
            let mut state = app_state.borrow_mut();
            match export {
                Ok(()) => {
                    state.playlist_io_status = format!("Exported to {}.", path.display());
                    state.feedback = "Playlist exported. Source media was left untouched.".into();
                }
                Err(error) => {
                    state.playlist_io_status = format!("Export failed: {error}");
                    state.feedback = state.playlist_io_status.clone();
                }
            }
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }
}

fn import_m3u8_into_state(state: &mut AppState, path: &std::path::Path) -> Result<String, String> {
    const MAX_PLAYLIST_BYTES: u64 = 2 * 1024 * 1024;
    let directory = path
        .parent()
        .ok_or_else(|| "Choose a playlist file inside a folder.".to_owned())?;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|error| format!("Unable to open playlist: {error}"))?
        .take(MAX_PLAYLIST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Unable to read playlist: {error}"))?;
    if bytes.len() as u64 > MAX_PLAYLIST_BYTES {
        return Err("The playlist exceeds the 2 MiB import limit.".into());
    }
    let entries =
        crate::playlists::parse_m3u8(&bytes, directory).map_err(|error| error.to_string())?;
    if entries.is_empty() {
        return Err("This playlist contains no media entries.".into());
    }

    let mut normalized_entries = Vec::with_capacity(entries.len());
    let mut new_local_paths = Vec::new();
    let local_tracks = state
        .tracks
        .iter()
        .chain(&state.movies)
        .filter(|track| track.source == TrackSource::Local);
    let mut known_local_paths = HashSet::new();
    for track in local_tracks {
        known_local_paths.insert(path_identity_key(&track.path));
        if let Ok(canonical) = std::fs::canonicalize(&track.path) {
            known_local_paths.insert(path_identity_key(&canonical));
        }
    }
    for entry in entries {
        match entry.source {
            crate::playlists::M3u8EntrySource::Local(source_path) => {
                if !source_path.is_file() {
                    return Err(format!(
                        "Playlist media is unavailable: {}",
                        source_path.display()
                    ));
                }
                let source_identity = path_identity_key(&source_path);
                let canonical = std::fs::canonicalize(&source_path).map_err(|error| {
                    format!(
                        "Unable to resolve playlist media {}: {error}",
                        source_path.display()
                    )
                })?;
                if !is_movie_path(&canonical) && !is_audio_path(&canonical) {
                    return Err(format!(
                        "Playlist media has an unsupported format: {}",
                        source_path.display()
                    ));
                }
                let canonical_identity = path_identity_key(&canonical);
                if !known_local_paths.contains(&source_identity)
                    && !known_local_paths.contains(&canonical_identity)
                {
                    new_local_paths.push(canonical.clone());
                }
                known_local_paths.insert(source_identity);
                known_local_paths.insert(canonical_identity);
                normalized_entries.push(crate::playlists::M3u8EntrySource::Local(canonical));
            }
            crate::playlists::M3u8EntrySource::Network(url) => {
                normalized_entries.push(crate::playlists::M3u8EntrySource::Network(url));
            }
        }
    }

    add_paths(state, new_local_paths, None);
    let mut local_tracks_by_path = HashMap::new();
    for track in state
        .tracks
        .iter()
        .chain(&state.movies)
        .filter(|track| track.source == TrackSource::Local)
    {
        local_tracks_by_path
            .entry(path_identity_key(&track.path))
            .or_insert(track.id);
        if let Ok(canonical) = std::fs::canonicalize(&track.path) {
            local_tracks_by_path
                .entry(path_identity_key(&canonical))
                .or_insert(track.id);
        }
    }
    let mut occurrences = Vec::with_capacity(normalized_entries.len());
    for source in normalized_entries {
        let track = match source {
            crate::playlists::M3u8EntrySource::Local(path) => local_tracks_by_path
                .get(&path_identity_key(&path))
                .and_then(|id| find_track(state, *id))
                .cloned()
                .ok_or_else(|| format!("Unable to add playlist media: {}", path.display()))?,
            crate::playlists::M3u8EntrySource::Network(url) => {
                ensure_network_track(state, &url, true).map_err(|error| error.to_string())?
            }
        };
        occurrences.push(track);
    }

    let base_name = path
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(safe_filename)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Imported playlist".into());
    let mut name = base_name.clone();
    let mut suffix = 2u32;
    while state
        .playlists
        .playlists()
        .iter()
        .any(|playlist| playlist.name.eq_ignore_ascii_case(&name))
    {
        name = format!("{base_name} ({suffix})");
        suffix = suffix.saturating_add(1);
    }
    let id = state
        .playlists
        .create(&name)
        .map_err(|error| error.to_string())?;
    state.playlist_target = Some(id);
    state.playlists.select(id);
    if let Err(error) = state
        .playlists
        .add_tracks_with_duplicates(&occurrences, true)
    {
        state.playlists.remove(id);
        state.playlist_target = state.playlists.selected_id();
        return Err(error.to_string());
    }
    state.current_view = "music".into();
    state.music_section = "playlists".into();
    state.opened_playlist = Some(id);
    state.playlist_rename_input = name.clone();
    state.page_index = 0;
    Ok(format!("Imported {} entries as {name}.", occurrences.len()))
}

fn is_audio_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|extension| {
            AUDIO_EXTENSIONS
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(extension))
        })
}

fn safe_filename(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control() && !matches!(*character, '/' | '\\' | ':'))
        .take(100)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn parse_control_identity(
    track_id: i32,
    load_generation: &str,
    playback: &PlaybackSnapshot,
) -> Option<(TrackId, u64)> {
    let load_generation = load_generation.parse::<u64>().ok()?;
    (playback.current_id == Some(track_id)
        && playback.load_generation == load_generation
        && playback.runtime_available)
        .then_some((track_id, load_generation))
}

fn ensure_network_track(
    state: &mut AppState,
    url: &NetworkUrl,
    audio_only: bool,
) -> Result<Track, String> {
    if let Some(track) = state
        .tracks
        .iter()
        .chain(&state.movies)
        .find(|track| {
            matches!(&track.source, TrackSource::Network(existing) if existing == url)
                && (track.format == "STREAM") == audio_only
        })
        .cloned()
    {
        return Ok(track);
    }
    let id = state.next_id;
    if id <= 0 || id == TrackId::MAX {
        return Err("This library has reached its media ID limit.".into());
    }
    let track = if audio_only {
        Track::from_network_audio_url(id, url.as_str())
    } else {
        Track::from_network_url(id, url.as_str())
    }
    .map_err(|error| error.to_string())?;
    state.next_id = state.next_id.saturating_add(1);
    if audio_only {
        state.tracks.push(track.clone());
    } else {
        state.movies.push(track.clone());
    }
    Ok(track)
}

fn persist_library(state: &mut AppState) {
    let Some(storage) = state.storage.as_ref() else {
        return;
    };
    let data = library_snapshot(state);
    if let Err(error) = storage.save(data) {
        state.library_status = format!("Library save failed: {error}");
        state.feedback = "Changes are still in memory but could not be queued for saving.".into();
    }
}

fn library_snapshot(state: &AppState) -> LibraryData {
    LibraryData {
        tracks: state.tracks.clone(),
        movies: state.movies.clone(),
        playlists: state.playlists.playlists().to_vec(),
        saved_movie_ids: {
            let mut ids = state.saved_movies.iter().copied().collect::<Vec<_>>();
            ids.sort_unstable();
            ids
        },
        selected_playlist: state.playlist_target,
        current_view: state.current_view.clone(),
        volume: state.volume,
        rate: state.rate,
        progress: state.progress.values().cloned().collect(),
        details: state
            .details
            .iter()
            .map(|(id, details)| (*id, details.clone()))
            .collect(),
        watched_roots: state.watched_roots.clone(),
        queue_checkpoint: state.queue_checkpoint.clone(),
        bookmarks: state.bookmarks.clone(),
        network_history: state.network_history.clone(),
        equalizer_settings: state.equalizer_settings,
    }
}

fn restore_queue_tracks(state: &AppState, checkpoint: &QueueCheckpoint) -> Vec<Track> {
    checkpoint
        .entry_ids
        .iter()
        .filter_map(|id| find_track(state, *id).cloned())
        .collect()
}

fn submit_queue_restore(
    state: &mut AppState,
    sender: &SyncSender<PlaybackCommand>,
    snapshot: &Arc<Mutex<PlaybackSnapshot>>,
) {
    if !state.queue_restore_pending {
        return;
    }
    if read_snapshot(snapshot).queue_restored {
        state.queue_restore_pending = false;
        return;
    }

    match send_command(
        sender,
        PlaybackCommand::RestoreQueue {
            tracks: restore_queue_tracks(state, &state.queue_checkpoint),
            checkpoint: state.queue_checkpoint.clone(),
        },
        snapshot,
    ) {
        Ok(()) => {
            state.queue_restore_pending = false;
            state.feedback.clear();
        }
        Err(error) if error == "Playback is busy. Wait a moment, then try again." => {
            state.feedback = error;
        }
        Err(error) => {
            state.queue_restore_pending = false;
            state.feedback = error;
        }
    }
}

fn update_queue_checkpoint(
    state: &mut AppState,
    playback: &PlaybackSnapshot,
    save_position: bool,
) -> bool {
    if !playback.queue_restored {
        return false;
    }
    state.queue_restore_pending = false;
    let current = &playback.queue_checkpoint;
    let previous = &state.queue_checkpoint;
    let structure_changed = previous.entry_ids != current.entry_ids
        || previous.order != current.order
        || previous.position != current.position
        || previous.wrapped_from_end != current.wrapped_from_end
        || previous.repeat_mode != current.repeat_mode
        || previous.shuffle != current.shuffle;
    let position_changed = previous.position_ms != current.position_ms;
    state.queue_checkpoint = current.clone();
    structure_changed || (save_position && position_changed)
}

fn submit_equalizer_restore(
    state: &mut AppState,
    sender: &SyncSender<PlaybackCommand>,
    snapshot: &Arc<Mutex<PlaybackSnapshot>>,
) {
    if !state.equalizer_restore_pending {
        return;
    }
    match send_command(
        sender,
        PlaybackCommand::SetEqualizerSettings(state.equalizer_settings),
        snapshot,
    ) {
        Ok(()) => {
            state.equalizer_restore_pending = false;
            state.equalizer_restore_sent_at = Some(Instant::now());
        }
        Err(error) if error == "Playback is busy. Wait a moment, then try again." => {}
        Err(error) => {
            if !state.equalizer_restore_fallback {
                state.equalizer_settings = EqualizerSettings::default();
                state.equalizer_restore_fallback = true;
                state.equalizer_settings_pending = true;
                state.equalizer_restore_sent_at = None;
                state.feedback = format!(
                    "Saved equalizer settings were rejected ({error}). Trying safe defaults."
                );
            } else {
                state.equalizer_restore_pending = false;
                state.equalizer_settings_pending = false;
                state.equalizer_restore_sent_at = None;
                state.equalizer_restore_failed = true;
                state.feedback = format!(
                    "Equalizer settings could not be applied ({error}); safe defaults remain saved."
                );
            }
        }
    }
}

fn reconcile_equalizer_settings(state: &mut AppState, playback: &PlaybackSnapshot) -> bool {
    if state.equalizer_restore_failed {
        return false;
    }
    if state.equalizer_editing {
        return false;
    }
    if state.equalizer_settings_pending {
        if playback.equalizer_settings == state.equalizer_settings {
            state.equalizer_settings_pending = false;
            state.equalizer_restore_sent_at = None;
            if state.equalizer_restore_fallback {
                state.equalizer_restore_fallback = false;
                state.feedback =
                    "Saved equalizer settings were unavailable; safe defaults were restored."
                        .into();
                return true;
            }
            return false;
        }
        if state
            .equalizer_restore_sent_at
            .is_some_and(|sent_at| sent_at.elapsed() >= Duration::from_secs(5))
        {
            state.equalizer_restore_sent_at = None;
            if !state.equalizer_restore_fallback {
                state.equalizer_settings = EqualizerSettings::default();
                state.equalizer_restore_fallback = true;
                state.equalizer_restore_pending = true;
                state.equalizer_settings_pending = true;
                state.feedback =
                    "Saved equalizer settings did not apply. Trying safe defaults instead.".into();
                return false;
            }
            state.equalizer_restore_pending = false;
            state.equalizer_settings_pending = false;
            state.equalizer_restore_failed = true;
            state.feedback =
                "Equalizer runtime did not acknowledge safe defaults; saved values were reset."
                    .into();
            return true;
        }
        if let Some(index) = state.equalizer_pending_preset {
            if playback.equalizer_settings.preset_index == Some(index) {
                state.equalizer_settings = playback.equalizer_settings;
                state.equalizer_settings_pending = false;
                state.equalizer_pending_preset = None;
                state.equalizer_restore_sent_at = None;
                return true;
            }
        }
        return false;
    }
    if state.equalizer_settings != playback.equalizer_settings {
        state.equalizer_settings = playback.equalizer_settings;
        return true;
    }
    false
}

fn update_network_history(state: &mut AppState, playback: &PlaybackSnapshot) -> bool {
    let active = matches!(
        playback.state,
        PlaybackUiState::Loading
            | PlaybackUiState::Playing
            | PlaybackUiState::Paused
            | PlaybackUiState::Ended
    );
    if !active {
        state.last_network_history_key = None;
        return false;
    }
    let Some(media_id) = playback.current_id else {
        state.last_network_history_key = None;
        return false;
    };
    let key = (media_id, playback.load_generation);
    if state.last_network_history_key == Some(key) {
        return false;
    }
    state.last_network_history_key = Some(key);
    let Some(track) = find_track(state, media_id) else {
        return false;
    };
    let TrackSource::Network(url) = &track.source else {
        return false;
    };
    let entry = NetworkHistoryEntry {
        url: url.clone(),
        title: network_history_title(&track.title),
        last_played_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
            .unwrap_or_default(),
    };
    state
        .network_history
        .retain(|existing| existing.url != entry.url);
    state.network_history.insert(0, entry);
    state.network_history.truncate(100);
    true
}

fn bookmark_name(input: &str, position_ms: i64) -> Result<String, &'static str> {
    if input.chars().any(char::is_control) {
        return Err("Bookmark names cannot contain control characters.");
    }
    let entered = input.trim();
    if entered.len() > MAX_BOOKMARK_NAME_BYTES {
        return Err("Bookmark names can be at most 128 UTF-8 bytes.");
    }
    Ok(if entered.is_empty() {
        format!("Point at {}", format_time(position_ms))
    } else {
        entered.to_owned()
    })
}

fn network_history_title(title: &str) -> String {
    let title = title
        .chars()
        .filter(|character| !character.is_control())
        .collect::<String>();
    let title = title.trim();
    let title = if title.is_empty() || title.contains("://") {
        "Network stream".to_owned()
    } else {
        title.to_owned()
    };
    let mut bounded = String::with_capacity(title.len().min(MAX_NETWORK_HISTORY_TITLE_BYTES));
    for character in title.chars() {
        if bounded.len() + character.len_utf8() > MAX_NETWORK_HISTORY_TITLE_BYTES {
            break;
        }
        bounded.push(character);
    }
    bounded
}

fn update_progress(state: &mut AppState, playback: &PlaybackSnapshot) {
    let Some(media_id) = playback.current_id else {
        return;
    };
    if playback.duration_ms <= 0 {
        return;
    }
    let position = playback.position_ms.clamp(0, playback.duration_ms);
    let completed = playback.state == PlaybackUiState::Ended
        || position.saturating_mul(100) >= playback.duration_ms.saturating_mul(95);
    let progress = Progress {
        media_id,
        position_ms: position,
        duration_ms: playback.duration_ms.max(0),
        completed,
    };
    if state.progress.get(&media_id) != Some(&progress) {
        state.progress.insert(media_id, progress);
    }
}

fn can_resume(progress: Option<&Progress>) -> bool {
    progress.is_some_and(|progress| {
        progress.position_ms > 10_000
            && progress.duration_ms > 0
            && !progress.completed
            && progress.position_ms.saturating_mul(100) < progress.duration_ms.saturating_mul(95)
    })
}

fn play_collection(
    state: &mut AppState,
    sender: &SyncSender<PlaybackCommand>,
    snapshot: &Arc<Mutex<PlaybackSnapshot>>,
    queue: Vec<Track>,
    start_index: usize,
    resume_position_ms: Option<i64>,
) -> bool {
    if queue.is_empty() || start_index >= queue.len() {
        return false;
    }
    let active_playlist_owner = playlist_owner_for_queue(state, &queue);
    let playback = read_snapshot(snapshot);
    update_progress(state, &playback);
    persist_library(state);
    match send_command(
        sender,
        PlaybackCommand::PlayQueue {
            tracks: queue,
            start_index,
            resume_position_ms,
        },
        snapshot,
    ) {
        Ok(()) => {
            state.active_playlist_owner = active_playlist_owner;
            state.feedback.clear();
            true
        }
        Err(error) => {
            state.feedback = error;
            false
        }
    }
}

fn play_collection_and_enter_video(
    weak: &slint::Weak<AppWindow>,
    state: &mut AppState,
    sender: &SyncSender<PlaybackCommand>,
    snapshot: &Arc<Mutex<PlaybackSnapshot>>,
    queue: Vec<Track>,
    start_index: usize,
    resume_position_ms: Option<i64>,
) -> bool {
    let starts_movie = collection_starts_movie(&queue, start_index);
    if !play_collection(
        state,
        sender,
        snapshot,
        queue,
        start_index,
        resume_position_ms,
    ) {
        return false;
    }
    if starts_movie {
        if let Some(ui) = weak.upgrade() {
            state.fullscreen_activity_at = Some(Instant::now());
            ui.set_movie_fullscreen(true);
            ui.window().set_fullscreen(true);
        }
    }
    true
}

fn process_library_events(
    state: &mut AppState,
    playback: &PlaybackSnapshot,
    sender: &SyncSender<PlaybackCommand>,
    snapshot: &Arc<Mutex<PlaybackSnapshot>>,
) {
    let events = state
        .library_worker
        .as_ref()
        .map(LibraryWorker::drain_events)
        .unwrap_or_default();
    for event in events {
        match event {
            LibraryEvent::Scanned(root, outcome) => {
                if state
                    .active_root_scan
                    .as_ref()
                    .is_some_and(|active| path_identity_key(active) == path_identity_key(&root))
                {
                    state.active_root_scan = None;
                }
                apply_scan_result(state, root, outcome);
            }
            LibraryEvent::Metadata(id, result) => {
                if state.tracks.iter().any(|track| track.id == id) {
                    state.automatic_artwork_readiness.insert(id, result.is_ok());
                }
                match result {
                    Ok(metadata) => {
                        if let Some(track) = state
                            .tracks
                            .iter_mut()
                            .chain(&mut state.movies)
                            .find(|track| track.id == id)
                        {
                            if let Some(title) = metadata
                                .title
                                .as_deref()
                                .filter(|title| !title.trim().is_empty())
                            {
                                track.title = title.to_owned();
                            }
                        }
                        apply_metadata_result(state, id, metadata);
                    }
                    Err(error) => {
                        state.feedback = format!("Unable to read local metadata: {error}")
                    }
                }
            }
            LibraryEvent::Artwork(id, generation, result) => {
                apply_artwork_result(state, id, generation, result);
            }
            LibraryEvent::AutomaticArtwork(id, request, result) => {
                if state.active_automatic_artwork == Some(id) {
                    state.active_automatic_artwork = None;
                }
                apply_automatic_artwork_result(state, id, &request, result);
            }
            LibraryEvent::SubtitlesFound(request_id, result) => {
                let Some(request) = state
                    .subtitle_search_pending
                    .filter(|request| request.request_id == request_id)
                else {
                    continue;
                };
                state.subtitle_search_pending = None;
                if playback.current_id != Some(request.media_id)
                    || playback.load_generation != request.load_generation
                {
                    state.subtitle_search_status =
                        "Search results were discarded because the media changed.".into();
                    continue;
                }
                match result {
                    Ok(results) => {
                        let count = results.len();
                        state.subtitle_results = results;
                        state.subtitle_search_status = if count == 0 {
                            "No matching subtitles found.".into()
                        } else {
                            format!("Found {count} subtitle result(s).")
                        };
                    }
                    Err(error) => {
                        state.subtitle_results.clear();
                        state.subtitle_search_status = error.clone();
                        state.feedback = error;
                    }
                }
            }
            LibraryEvent::SubtitleDownloaded(request_id, result) => {
                let Some(request) = state
                    .subtitle_download_pending
                    .filter(|request| request.request_id == request_id)
                else {
                    continue;
                };
                state.subtitle_download_pending = None;
                match result {
                    Ok(path)
                        if playback.current_id == Some(request.media_id)
                            && playback.load_generation == request.load_generation =>
                    {
                        match send_command(
                            sender,
                            PlaybackCommand::AddSubtitle {
                                media_id: request.media_id,
                                load_generation: request.load_generation,
                                path,
                            },
                            snapshot,
                        ) {
                            Ok(()) => {
                                state.subtitle_search_status =
                                    "Subtitle downloaded and attached.".into();
                                state.feedback.clear();
                            }
                            Err(error) => {
                                state.subtitle_search_status = error.clone();
                                state.feedback = error;
                            }
                        }
                    }
                    Ok(_) => {
                        state.subtitle_search_status = "Subtitle downloaded to cache, but media changed before it could be attached.".into();
                    }
                    Err(error) => {
                        state.subtitle_search_status = error.clone();
                        state.feedback = error;
                    }
                }
            }
        }
    }
}

fn schedule_automatic_artwork(state: &mut AppState) {
    if state.active_automatic_artwork.is_some() {
        return;
    }

    loop {
        if state
            .automatic_artwork_retries
            .front()
            .is_some_and(|id| !state.tracks.iter().any(|track| track.id == *id))
        {
            state.automatic_artwork_retries.pop_front();
            continue;
        }
        let retry = state.automatic_artwork_retries.front().copied();
        let id = match retry {
            Some(id) => id,
            None => match state.tracks.get(state.automatic_artwork_cursor) {
                Some(track) => track.id,
                None => return,
            },
        };
        let Some(metadata_read_ok) = state.automatic_artwork_readiness.get(&id).copied() else {
            return;
        };
        if !metadata_read_ok {
            state.automatic_artwork_readiness.remove(&id);
            if retry.is_some() {
                state.automatic_artwork_retries.pop_front();
            } else {
                state.automatic_artwork_cursor += 1;
            }
            continue;
        }

        let Some(track) = state.tracks.iter().find(|track| track.id == id) else {
            state.automatic_artwork_retries.pop_front();
            continue;
        };
        let details = state.details.get(&id).cloned().unwrap_or_default();
        let metadata = LocalMetadata {
            title: Some(track.title.clone()),
            artist: Some(details.artist),
            album: Some(details.album),
            album_artist: Some(details.album_artist),
            genre: Some(details.genre),
            track_number: details.track_number,
            disc_number: details.disc_number,
            artwork: details.artwork,
        };
        let Some(request) = crate::metadata::automatic_music_artwork_request(&metadata) else {
            state.automatic_artwork_readiness.remove(&id);
            if retry.is_some() {
                state.automatic_artwork_retries.pop_front();
            } else {
                state.automatic_artwork_cursor += 1;
            }
            continue;
        };

        let Some(worker) = state.library_worker.as_ref() else {
            state.feedback =
                "Automatic music artwork is unavailable because the lookup worker stopped.".into();
            return;
        };
        match worker.try_submit_automatic_artwork(id, request) {
            Ok(()) => {
                state.automatic_artwork_readiness.remove(&id);
                if retry.is_some() {
                    state.automatic_artwork_retries.pop_front();
                } else {
                    state.automatic_artwork_cursor += 1;
                }
                state.active_automatic_artwork = Some(id);
            }
            Err(crate::library_worker::QueueSubmitError::Full) => return,
            Err(crate::library_worker::QueueSubmitError::Stopped) => {
                state.feedback =
                    "Automatic music artwork is unavailable because the lookup worker stopped."
                        .into();
            }
        }
        return;
    }
}

fn queue_automatic_artwork_retry(state: &mut AppState, id: TrackId) -> bool {
    let Some(index) = state.tracks.iter().position(|track| track.id == id) else {
        return false;
    };
    if index < state.automatic_artwork_cursor && !state.automatic_artwork_retries.contains(&id) {
        state.automatic_artwork_retries.push_back(id);
    }
    true
}

fn next_artwork_intent(state: &mut AppState, id: TrackId) -> u64 {
    let generation = state.next_artwork_generation.max(1);
    state.next_artwork_generation = generation.wrapping_add(1).max(1);
    state.artwork_intents.insert(id, generation);
    generation
}

fn apply_artwork_result(
    state: &mut AppState,
    id: TrackId,
    generation: u64,
    result: Result<Option<PathBuf>, String>,
) -> bool {
    if state.artwork_intents.get(&id) != Some(&generation) {
        return false;
    }

    match result {
        Ok(Some(path)) => {
            let details = state.details.entry(id).or_default();
            details.artwork = Some(path);
            details.artwork_override = true;
            state.feedback = "Artwork saved in the local cache and selected for this item.".into();
            persist_library(state);
        }
        Ok(None) => state.feedback = "No matching artwork was found.".into(),
        Err(error) => state.feedback = format!("Artwork lookup failed: {error}"),
    }
    true
}

fn apply_automatic_artwork_result(
    state: &mut AppState,
    id: TrackId,
    request: &ArtworkRequest,
    result: Result<Option<PathBuf>, String>,
) -> bool {
    let Some(track) = state.tracks.iter().find(|track| track.id == id) else {
        return false;
    };
    let Some(details) = state.details.get_mut(&id) else {
        return false;
    };
    if request.kind != ArtworkKind::Music
        || !crate::model::is_supported_audio_path(&track.path)
        || track.title.trim() != request.title.trim()
        || details.artist.trim() != request.artist.as_deref().unwrap_or("").trim()
        || details.album.trim() != request.album.as_deref().unwrap_or("").trim()
        || details.artwork_override
        || details.artwork.is_some()
    {
        return false;
    }
    let Ok(Some(path)) = result else {
        return false;
    };
    details.artwork = Some(path);
    persist_library(state);
    true
}

fn apply_metadata_result(state: &mut AppState, id: TrackId, metadata: LocalMetadata) {
    let details = state.details.entry(id).or_default();
    if let Some(value) = metadata.artist {
        details.artist = value;
    }
    if let Some(value) = metadata.album {
        details.album = value;
    }
    if let Some(value) = metadata.album_artist {
        details.album_artist = value;
    }
    if let Some(value) = metadata.genre {
        details.genre = value;
    }
    if metadata.disc_number.is_some() {
        details.disc_number = metadata.disc_number;
    }
    if metadata.track_number.is_some() {
        details.track_number = metadata.track_number;
    }
    if !details.artwork_override {
        details.artwork = metadata.artwork;
    }
    persist_library(state);
}

const ROOT_SCAN_INTERVAL: Duration = Duration::from_secs(60);

fn add_watched_root(state: &mut AppState, selected: PathBuf) -> Result<bool, String> {
    let root = selected
        .canonicalize()
        .map_err(|error| format!("Unable to resolve the selected folder: {error}"))?;
    if !root.is_dir() {
        return Err("The selected path is not a folder.".into());
    }
    let identity = path_identity_key(&root);
    if let Some(existing) = state
        .watched_roots
        .iter()
        .find(|watched| {
            let canonical = watched
                .canonicalize()
                .unwrap_or_else(|_| watched.to_path_buf());
            path_identity_key(&canonical) == identity
        })
        .cloned()
    {
        queue_root_scan(state, existing);
        return Ok(false);
    }
    state.watched_roots.push(root.clone());
    state.root_scan_status.insert(identity, "Queued".into());
    queue_root_scan(state, root);
    persist_library(state);
    Ok(true)
}

fn remove_watched_root(state: &mut AppState, index: usize) -> Option<PathBuf> {
    if index >= state.watched_roots.len() {
        return None;
    }
    let root = state.watched_roots.remove(index);
    let identity = path_identity_key(&root);
    state
        .pending_root_scans
        .retain(|pending| path_identity_key(pending) != identity);
    state.root_scan_status.remove(&identity);
    persist_library(state);
    Some(root)
}

fn queue_root_scan(state: &mut AppState, root: PathBuf) {
    let identity = path_identity_key(&root);
    let watched = state
        .watched_roots
        .iter()
        .any(|watched| path_identity_key(watched) == identity);
    let active = state
        .active_root_scan
        .as_ref()
        .is_some_and(|active| path_identity_key(active) == identity);
    let pending = state
        .pending_root_scans
        .iter()
        .any(|pending| path_identity_key(pending) == identity);
    if watched && !active && !pending {
        state.pending_root_scans.push_back(root.clone());
        state.root_scan_status.insert(identity, "Queued".into());
    }
}

fn schedule_periodic_root_scans(state: &mut AppState) {
    if state.last_scheduled_root_scan.elapsed() < ROOT_SCAN_INTERVAL {
        return;
    }
    let roots = state.watched_roots.clone();
    for root in roots {
        queue_root_scan(state, root);
    }
    state.last_scheduled_root_scan = Instant::now();
}

fn flush_pending_root_scans(state: &mut AppState) {
    if state.active_root_scan.is_some() {
        return;
    }
    while let Some(root) = state.pending_root_scans.front().cloned() {
        let identity = path_identity_key(&root);
        if !state
            .watched_roots
            .iter()
            .any(|watched| path_identity_key(watched) == identity)
        {
            state.pending_root_scans.pop_front();
            continue;
        }
        let Some(worker) = state.library_worker.as_ref() else {
            state
                .root_scan_status
                .insert(identity, "Scanning unavailable".into());
            state.pending_root_scans.clear();
            return;
        };
        match worker.try_submit(LibraryJob::Scan(root.clone())) {
            Ok(()) => {
                state.pending_root_scans.pop_front();
                state.active_root_scan = Some(root.clone());
                state.root_scan_status.insert(identity, "Scanning".into());
            }
            Err(crate::library_worker::QueueSubmitError::Full) => return,
            Err(crate::library_worker::QueueSubmitError::Stopped) => {
                state
                    .root_scan_status
                    .insert(identity, "Scanning unavailable".into());
                state.pending_root_scans.clear();
                state.library_status =
                    "Folder scanning stopped. Restart the application to scan watched folders."
                        .into();
                return;
            }
        }
        return;
    }
}

fn apply_scan_result(state: &mut AppState, root: PathBuf, outcome: ScanOutcome) {
    let root_identity = path_identity_key(&root);
    if !state
        .watched_roots
        .iter()
        .any(|watched| path_identity_key(watched) == root_identity)
    {
        return;
    }
    let found = outcome
        .paths
        .iter()
        .map(|path| path_identity_key(path))
        .collect::<HashSet<_>>();
    let track_paths = state
        .tracks
        .iter()
        .chain(&state.movies)
        .filter(|track| track.source == TrackSource::Local)
        .map(|track| (path_identity_key(&track.path), track.id))
        .collect::<Vec<_>>();
    let mut track_ids_by_path = HashMap::new();
    for (path, track_id) in &track_paths {
        track_ids_by_path.entry(path.clone()).or_insert(*track_id);
    }
    let mut changed_ids = Vec::new();
    for fingerprint in &outcome.fingerprints {
        let identity = path_identity_key(&fingerprint.path);
        if let Some(track_id) = track_ids_by_path.get(&identity) {
            if state
                .file_fingerprints
                .get(&identity)
                .is_some_and(|previous| file_fingerprint_changed(previous, fingerprint))
            {
                changed_ids.push(*track_id);
            }
        }
        state
            .file_fingerprints
            .insert(identity, fingerprint.clone());
    }
    if outcome.complete {
        for (path, track_id) in &track_paths {
            if path.starts_with(&root_identity) {
                if found.contains(path) {
                    state.unavailable_media.remove(track_id);
                } else {
                    state.unavailable_media.insert(*track_id);
                }
            }
        }
        state.root_scan_status.insert(
            root_identity.clone(),
            format!("Scanned · {} media files", outcome.paths.len()),
        );
    } else {
        let detail = outcome
            .warnings
            .first()
            .map(String::as_str)
            .unwrap_or("Scan incomplete; existing library entries were retained.");
        state
            .root_scan_status
            .insert(root_identity, format!("Incomplete: {detail}"));
    }
    changed_ids.sort_unstable();
    changed_ids.dedup();
    for id in changed_ids {
        if !state.pending_metadata.contains(&id) {
            state.pending_metadata.push_back(id);
        }
    }
    let (count, metadata_available) = add_paths(state, outcome.paths, None);
    flush_pending_metadata(state);
    if count > 0 {
        state.current_view = if !state.movies.is_empty() {
            "movies"
        } else {
            "music"
        }
        .into();
        state.movie_level = "catalog".into();
        state.page_index = 0;
        if metadata_available {
            state.feedback = format!(
                "Added {count} media file(s). Metadata and local artwork are loading in the background."
            );
        }
        persist_library(state);
    }
    if !outcome.complete {
        let detail = outcome
            .warnings
            .first()
            .map(String::as_str)
            .unwrap_or("The scan reached a safety limit.");
        state.feedback = format!(
            "{} Some folders or entries were skipped: {detail}",
            state.feedback
        );
    }
}

fn file_fingerprint_changed(
    previous: &crate::scan::FileFingerprint,
    current: &crate::scan::FileFingerprint,
) -> bool {
    previous.size != current.size
        || match (previous.modified, current.modified) {
            (Some(previous), Some(current)) => previous != current,
            _ => true,
        }
}

#[allow(clippy::too_many_arguments)]
fn queue_command(
    state: &Rc<RefCell<AppState>>,
    sender: &SyncSender<PlaybackCommand>,
    snapshot: &Arc<Mutex<PlaybackSnapshot>>,
    weak: &slint::Weak<AppWindow>,
    revision: &str,
    position: i32,
    target: Option<i32>,
    operation: QueueOperation,
) {
    let mut state = state.borrow_mut();
    let revision = revision.parse::<u64>();
    let position = usize::try_from(position);
    let result = match (
        revision.ok(),
        position.ok(),
        target.map(usize::try_from).transpose().ok().flatten(),
    ) {
        (Some(revision), Some(position), target) => {
            let command = match operation {
                QueueOperation::Jump => Some(PlaybackCommand::QueueJump { revision, position }),
                QueueOperation::Move => target.map(|to| PlaybackCommand::QueueMove {
                    revision,
                    from: position,
                    to,
                }),
                QueueOperation::Remove => Some(PlaybackCommand::QueueRemove { revision, position }),
            };
            command
                .map(|command| send_command(sender, command, snapshot))
                .unwrap_or_else(|| Err("Invalid queue position.".into()))
        }
        _ => Err("That queue row is stale. Refresh the queue and try again.".into()),
    };
    if result.is_ok() && !matches!(operation, QueueOperation::Jump) {
        state.active_playlist_owner = None;
    }
    if let Err(error) = result {
        state.feedback = error;
    }
    refresh_from_parts(weak, &mut state, snapshot);
}

fn music_collection(state: &AppState) -> Vec<Track> {
    let mut tracks = if state.music_section == "playlists" {
        state
            .opened_playlist
            .map(|id| playlist_collection_for(state, id, &state.query, &state.music_genre))
            .unwrap_or_default()
    } else {
        let group = if state.music_section == "tracks" {
            None
        } else {
            state.open_music_group
        };
        browse::collection(
            &state.tracks,
            &state.details,
            &state.music_section,
            group,
            None,
            &state.query,
        )
    };
    if state.music_genre != "All genres" {
        tracks.retain(|track| genre_matches(&state.details, track.id, &state.music_genre));
    }
    let opened_playlist = state.current_view == "music"
        && state.music_section == "playlists"
        && state.opened_playlist.is_some();
    let album_disc_track_default = state.music_section == "albums"
        && state.open_music_group.is_some()
        && (!state.music_sort_explicit || state.music_sort == "album");
    if !opened_playlist && !album_disc_track_default {
        sort_music_tracks(&mut tracks, state);
    }
    tracks
}

#[cfg(test)]
fn playlist_collection(state: &AppState, id: PlaylistId, query: &str) -> Vec<Track> {
    playlist_collection_for(state, id, query, &state.music_genre)
}

fn playlist_collection_for(
    state: &AppState,
    id: PlaylistId,
    query: &str,
    genre: &str,
) -> Vec<Track> {
    let tracks = state
        .tracks
        .iter()
        .chain(&state.movies)
        .cloned()
        .collect::<Vec<_>>();
    let playlist = state
        .playlists
        .playlists()
        .iter()
        .find(|playlist| playlist.id == id);
    let mut tracks =
        browse::collection(&tracks, &state.details, "playlists", None, playlist, query);
    if genre != "All genres" {
        tracks.retain(|track| genre_matches(&state.details, track.id, genre));
    }
    tracks
}

fn playlist_filtered_positions(state: &AppState, id: PlaylistId, query: &str) -> Vec<usize> {
    let Some(playlist) = state
        .playlists
        .playlists()
        .iter()
        .find(|playlist| playlist.id == id)
    else {
        return Vec::new();
    };
    let tracks: HashMap<_, _> = state
        .tracks
        .iter()
        .chain(&state.movies)
        .map(|track| (track.id, track))
        .collect();
    playlist
        .track_ids
        .iter()
        .enumerate()
        .filter_map(|(position, track_id)| {
            let track = tracks.get(track_id)?;
            (browse::matches_query(track, state.details.get(track_id), query)
                && (state.music_genre == "All genres"
                    || genre_matches(&state.details, *track_id, &state.music_genre)))
            .then_some(position)
        })
        .collect()
}

fn playlist_row_is_current(
    state: &AppState,
    playlist_id: PlaylistId,
    display_index: i32,
    playlist_position: i32,
    track_id: TrackId,
) -> bool {
    if state.current_view != "music"
        || state.music_section != "playlists"
        || state.opened_playlist != Some(playlist_id)
    {
        return false;
    }
    let (Ok(display_index), Ok(playlist_position)) = (
        usize::try_from(display_index),
        usize::try_from(playlist_position),
    ) else {
        return false;
    };
    let page_start = state.page_index.saturating_mul(MUSIC_PAGE_SIZE);
    let Some(page_index) = display_index.checked_sub(page_start) else {
        return false;
    };
    state
        .visible_tracks
        .get(page_index)
        .is_some_and(|track| track.id == track_id)
        && state.visible_playlist_positions.get(page_index) == Some(&playlist_position)
        && state.playlists.playlists().iter().any(|playlist| {
            playlist.id == playlist_id
                && playlist.track_ids.get(playlist_position) == Some(&track_id)
        })
        && playlist_filtered_positions(state, playlist_id, &state.query).get(display_index)
            == Some(&playlist_position)
}

fn playlist_move_target(
    positions: &[usize],
    display_index: usize,
    direction: i32,
) -> Option<usize> {
    let target_index = match direction {
        -1 => display_index.checked_sub(1)?,
        1 => display_index.checked_add(1)?,
        _ => return None,
    };
    positions.get(target_index).copied()
}

fn playlist_owner_for_queue(state: &AppState, queue: &[Track]) -> Option<ActivePlaylistOwner> {
    if state.current_view != "music" || state.music_section != "playlists" {
        return None;
    }
    let playlist_id = state.opened_playlist?;
    let displayed = playlist_collection_for(state, playlist_id, &state.query, &state.music_genre);
    (displayed == queue).then(|| ActivePlaylistOwner {
        playlist_id,
        query: state.query.clone(),
        genre: state.music_genre.clone(),
    })
}

fn selected_displayed_tracks(state: &AppState) -> Vec<Track> {
    if state.current_view == "music" && state.music_section == "playlists" {
        if let Some(playlist_id) = state.opened_playlist {
            return state
                .visible_tracks
                .iter()
                .zip(&state.visible_playlist_positions)
                .filter(|(_, position)| {
                    state
                        .playlist_selection
                        .contains(&(playlist_id, **position))
                })
                .map(|(track, _)| track.clone())
                .collect();
        }
    }
    state
        .visible_tracks
        .iter()
        .filter(|track| state.selection.contains(track.id))
        .cloned()
        .collect()
}

fn collection_start_index(queue: &[Track], id: TrackId, requested_index: i32) -> Option<usize> {
    usize::try_from(requested_index)
        .ok()
        .filter(|index| queue.get(*index).is_some_and(|track| track.id == id))
        .or_else(|| queue.iter().position(|track| track.id == id))
}

fn collection_track_at(queue: &[Track], id: TrackId, requested_index: i32) -> Option<&Track> {
    usize::try_from(requested_index)
        .ok()
        .and_then(|index| queue.get(index))
        .filter(|track| track.id == id)
}

fn collection_starts_movie(queue: &[Track], start_index: usize) -> bool {
    queue
        .get(start_index)
        .is_some_and(crate::playback::is_movie)
}

fn movie_matches(state: &AppState, track: &Track) -> bool {
    if !browse::matches_query(track, state.details.get(&track.id), &state.query) {
        return false;
    }
    if state.movie_view == "my-list" && !state.saved_movies.contains(&track.id) {
        return false;
    }
    if state.movie_genre != "All genres"
        && !genre_matches(&state.details, track.id, &state.movie_genre)
    {
        return false;
    }
    let progress = state.progress.get(&track.id);
    match state.movie_filter.as_str() {
        "continue" => can_resume(progress),
        "completed" => progress.is_some_and(|p| {
            p.completed
                || (p.duration_ms > 0
                    && p.position_ms.saturating_mul(100) >= p.duration_ms.saturating_mul(95))
        }),
        "unwatched" => progress.is_none_or(|p| !p.completed && p.position_ms <= 10_000),
        _ => true,
    }
}

fn path_identity_key(path: &Path) -> PathIdentityKey {
    #[cfg(windows)]
    let path = {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};

        let units = path.as_os_str().encode_wide().collect::<Vec<_>>();
        PathBuf::from(OsString::from_wide(&windows_path_identity_units(&units)))
    };
    #[cfg(not(windows))]
    let path = path;

    path.components()
        .map(|component| component.as_os_str().to_os_string())
        .collect()
}

#[cfg(any(windows, test))]
fn windows_path_identity_units(path: &[u16]) -> Vec<u16> {
    const VERBATIM_PREFIX: [u16; 4] = [b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];

    let Some(remainder) = path.strip_prefix(&VERBATIM_PREFIX) else {
        return path.to_vec();
    };
    if wide_prefix_eq_ascii(remainder, b"UNC\\") {
        let mut normalized = vec![b'\\' as u16, b'\\' as u16];
        normalized.extend_from_slice(&remainder[4..]);
        return normalized;
    }
    if remainder.len() >= 3
        && remainder[0] <= u8::MAX as u16
        && (remainder[0] as u8).is_ascii_alphabetic()
        && remainder[1] == b':' as u16
        && (remainder[2] == b'\\' as u16 || remainder[2] == b'/' as u16)
    {
        return remainder.to_vec();
    }
    path.to_vec()
}

#[cfg(any(windows, test))]
fn wide_prefix_eq_ascii(value: &[u16], prefix: &[u8]) -> bool {
    value.len() >= prefix.len()
        && value.iter().zip(prefix).all(|(&unit, expected)| {
            unit <= u8::MAX as u16 && (unit as u8).eq_ignore_ascii_case(expected)
        })
}

fn movie_collection(state: &AppState) -> Vec<Track> {
    let mut movies: Vec<Track> = state
        .movies
        .iter()
        .filter(|track| movie_matches(state, track))
        .cloned()
        .collect();
    if state.movie_level == "episodes" {
        let series = state.selected_series.as_deref().unwrap_or_default();
        let season = state.selected_season;
        movies.retain(|track| {
            episode_info(&track.path)
                .is_some_and(|episode| episode.series == series && Some(episode.season) == season)
        });
        movies.sort_by_key(|track| {
            episode_info(&track.path)
                .map(|episode| (episode.season, episode.episode))
                .unwrap_or((u32::MAX, u32::MAX))
        });
    } else {
        movies.retain(|track| episode_info(&track.path).is_none());
        sort_movie_tracks(&mut movies, state.movie_sort.as_str());
    }
    movies
}

fn process_pending_movie_details(state: &mut AppState, now: Instant) {
    let Some((id, clicked_at)) = state.pending_movie_details else {
        return;
    };
    if now.duration_since(clicked_at) < MOVIE_DETAILS_CLICK_DELAY {
        return;
    }
    state.pending_movie_details = None;
    if state.current_view == "movies"
        && movie_collection(state).iter().any(|movie| movie.id == id)
        && set_movie_artwork_target(state, id)
    {
        state.selected_movie = Some(id);
        state.movie_details_open = true;
    }
}

fn movie_double_click(pending: Option<(TrackId, Instant)>, id: TrackId, now: Instant) -> bool {
    pending.is_some_and(|(pending_id, clicked_at)| {
        pending_id == id && now.duration_since(clicked_at) <= MOVIE_DETAILS_CLICK_DELAY
    })
}

fn genre_names(value: &str) -> impl Iterator<Item = &str> {
    value
        .split([',', ';', '|'])
        .map(str::trim)
        .filter(|genre| !genre.is_empty())
}

fn genre_matches(details: &HashMap<TrackId, MediaDetails>, id: TrackId, filter: &str) -> bool {
    details.get(&id).is_some_and(|details| {
        genre_names(&details.genre).any(|genre| genre.eq_ignore_ascii_case(filter))
    })
}

fn available_genres(
    tracks: &[Track],
    details: &HashMap<TrackId, MediaDetails>,
    current: &str,
) -> Vec<SharedString> {
    let mut genres = BTreeMap::<String, String>::new();
    for track in tracks {
        if let Some(details) = details.get(&track.id) {
            for genre in genre_names(&details.genre) {
                genres
                    .entry(genre.to_lowercase())
                    .or_insert_with(|| genre.to_owned());
            }
        }
    }
    if current != "All genres" {
        genres
            .entry(current.to_lowercase())
            .or_insert_with(|| current.to_owned());
    }
    std::iter::once(SharedString::from("All genres"))
        .chain(genres.into_values().map(SharedString::from))
        .collect()
}

fn movie_sort_key(label: &str) -> &'static str {
    match label {
        "Title A–Z" => "title",
        "Year newest" => "year-desc",
        "Year oldest" => "year-asc",
        _ => "recent",
    }
}

fn music_sort_key(label: &str) -> &'static str {
    match label {
        "Disc / track" => "album",
        "Title A–Z" => "title",
        "Artist A–Z" => "artist",
        _ => "recent",
    }
}

fn sort_movie_tracks(movies: &mut [Track], sort: &str) {
    movies.sort_by(|left, right| {
        let title_order = || left.title.to_lowercase().cmp(&right.title.to_lowercase());
        match sort {
            "title" => title_order().then_with(|| left.id.cmp(&right.id)),
            "year-desc" => release_year(&right.title)
                .unwrap_or(0)
                .cmp(&release_year(&left.title).unwrap_or(0))
                .then_with(title_order),
            "year-asc" => release_year(&left.title)
                .unwrap_or(u16::MAX)
                .cmp(&release_year(&right.title).unwrap_or(u16::MAX))
                .then_with(title_order),
            _ => right.id.cmp(&left.id),
        }
    });
}

fn release_year(title: &str) -> Option<u16> {
    let title = title.trim();
    let (open, close) = match title.chars().last()? {
        ')' => ('(', ')'),
        ']' => ('[', ']'),
        _ => return None,
    };
    let start = title.rfind(open)? + open.len_utf8();
    let year = title[start..title.len() - close.len_utf8()]
        .parse::<u16>()
        .ok()?;
    (!title[..start - open.len_utf8()].trim_end().is_empty() && (1870..=2100).contains(&year))
        .then_some(year)
}

fn sort_music_tracks(tracks: &mut [Track], state: &AppState) {
    tracks.sort_by(|left, right| match state.music_sort.as_str() {
        "title" => left
            .title
            .to_lowercase()
            .cmp(&right.title.to_lowercase())
            .then_with(|| left.id.cmp(&right.id)),
        "artist" => {
            let artist = |track: &Track| {
                state
                    .details
                    .get(&track.id)
                    .map(|details| nonempty(&details.album_artist, &details.artist).unwrap_or(""))
                    .unwrap_or("")
                    .to_lowercase()
            };
            artist(left)
                .cmp(&artist(right))
                .then_with(|| left.title.to_lowercase().cmp(&right.title.to_lowercase()))
                .then_with(|| left.id.cmp(&right.id))
        }
        _ => right.id.cmp(&left.id),
    });
}

fn is_movie_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|extension| {
            MOVIE_EXTENSIONS
                .iter()
                .any(|allowed| extension.eq_ignore_ascii_case(allowed))
        })
}

fn monogram(title: &str) -> SharedString {
    SharedString::from(
        title
            .chars()
            .find(|character| character.is_alphanumeric())
            .map(|character| character.to_uppercase().to_string())
            .unwrap_or_else(|| "•".into()),
    )
}

fn nonempty<'a>(first: &'a str, second: &'a str) -> Option<&'a str> {
    [first, second]
        .into_iter()
        .find(|value| !value.trim().is_empty())
}

fn progress_label(progress: Option<&Progress>) -> String {
    let Some(progress) = progress else {
        return String::new();
    };
    if can_resume(Some(progress)) {
        format!(
            "Continue · {} / {}",
            format_time(progress.position_ms),
            format_time(progress.duration_ms)
        )
    } else if progress.completed
        || (progress.position_ms > 10_000
            && progress.duration_ms > 0
            && progress.position_ms.saturating_mul(100) >= progress.duration_ms.saturating_mul(95))
    {
        "Completed".into()
    } else {
        String::new()
    }
}

fn artwork_target(state: &AppState, playback: &PlaybackSnapshot) -> Option<TrackId> {
    state
        .artwork_target
        .filter(|id| find_track(state, *id).is_some())
        .or(playback
            .current_id
            .filter(|id| find_track(state, *id).is_some()))
}

fn set_movie_artwork_target(state: &mut AppState, id: TrackId) -> bool {
    if state.movies.iter().any(|movie| movie.id == id) {
        state.artwork_target = Some(id);
        true
    } else {
        state.artwork_target = None;
        false
    }
}

fn cached_image(state: &mut AppState, path: Option<&PathBuf>) -> Option<Image> {
    let path = path?;
    if let Some(index) = state
        .image_cache
        .iter()
        .position(|(cached, _)| cached == path)
    {
        let entry = state.image_cache.remove(index)?;
        let image = entry.1.clone();
        state.image_cache.push_front(entry);
        return Some(image);
    }
    let decoded = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    let thumbnail = decoded.thumbnail(256, 256).to_rgba8();
    let (width, height) = thumbnail.dimensions();
    let pixels =
        SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(thumbnail.as_raw(), width, height);
    let image = Image::from_rgba8(pixels);
    state.image_cache.push_front((path.clone(), image.clone()));
    while state.image_cache.len() > MUSIC_PAGE_SIZE {
        state.image_cache.pop_back();
    }
    Some(image)
}

fn parse_delay_us(value: &str) -> Option<i64> {
    let milliseconds = value.trim().parse::<i64>().ok()?;
    let microseconds = milliseconds.checked_mul(1000)?;
    (-MAX_MEDIA_DELAY_US..=MAX_MEDIA_DELAY_US)
        .contains(&microseconds)
        .then_some(microseconds)
}

fn parse_time_ms(value: &str) -> Option<i64> {
    let parts: Vec<&str> = value.trim().split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    let (whole, millis) = parts
        .last()?
        .split_once('.')
        .map(|(whole, fraction)| (whole, Some(fraction)))
        .unwrap_or((*parts.last()?, None));
    let mut seconds = whole.parse::<i64>().ok()?;
    if let Some(fraction) = millis {
        if fraction.is_empty() || !fraction.chars().all(|character| character.is_ascii_digit()) {
            return None;
        }
        let normalized = match fraction.len() {
            1 => format!("{fraction}00"),
            2 => format!("{fraction}0"),
            _ => fraction[..3].to_owned(),
        };
        let millis = normalized.parse::<i64>().ok()?;
        seconds = seconds.checked_mul(1000)?.checked_add(millis)?;
    } else {
        seconds = seconds.checked_mul(1000)?;
    }
    for (index, part) in parts[..parts.len() - 1].iter().rev().enumerate() {
        let field = part.parse::<i64>().ok()?;
        if !(0..60).contains(&field) {
            return None;
        }
        let multiplier = if index == 0 { 60_000 } else { 3_600_000 };
        seconds = seconds.checked_add(field.checked_mul(multiplier)?)?;
    }
    (seconds >= 0).then_some(seconds)
}

fn register_transport(
    ui: &AppWindow,
    app_state: &Rc<RefCell<AppState>>,
    sender: &SyncSender<PlaybackCommand>,
    snapshot: &Arc<Mutex<PlaybackSnapshot>>,
) {
    {
        let app_state = Rc::clone(app_state);
        let sender = sender.clone();
        let snapshot = Arc::clone(snapshot);
        let weak = ui.as_weak();
        ui.on_previous(move || {
            let mut state = app_state.borrow_mut();
            update_progress(&mut state, &read_snapshot(&snapshot));
            submit_transport_command(&mut state, &sender, &snapshot, PlaybackCommand::Previous);
            persist_library(&mut state);
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }

    {
        let app_state = Rc::clone(app_state);
        let sender = sender.clone();
        let snapshot = Arc::clone(snapshot);
        let weak = ui.as_weak();
        ui.on_toggle_pause(move || {
            let mut state = app_state.borrow_mut();
            update_progress(&mut state, &read_snapshot(&snapshot));
            submit_transport_command(&mut state, &sender, &snapshot, PlaybackCommand::TogglePause);
            persist_library(&mut state);
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }

    {
        let app_state = Rc::clone(app_state);
        let sender = sender.clone();
        let snapshot = Arc::clone(snapshot);
        let weak = ui.as_weak();
        ui.on_next(move || {
            let mut state = app_state.borrow_mut();
            update_progress(&mut state, &read_snapshot(&snapshot));
            submit_transport_command(&mut state, &sender, &snapshot, PlaybackCommand::Next);
            persist_library(&mut state);
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }

    {
        let app_state = Rc::clone(app_state);
        let sender = sender.clone();
        let snapshot = Arc::clone(snapshot);
        let weak = ui.as_weak();
        ui.on_stop(move || {
            let mut state = app_state.borrow_mut();
            update_progress(&mut state, &read_snapshot(&snapshot));
            state.pending_seek = None;
            if let Some(ui) = weak.upgrade() {
                ui.set_adjusting_seek(false);
            }
            submit_transport_command(&mut state, &sender, &snapshot, PlaybackCommand::Stop);
            persist_library(&mut state);
            refresh_from_parts(&weak, &mut state, &snapshot);
        });
    }

    {
        let app_state = Rc::clone(app_state);
        let sender = sender.clone();
        let snapshot = Arc::clone(snapshot);
        let weak = ui.as_weak();
        ui.on_seek_changed(move |position_ms, track_id, load_generation, committed| {
            let mut state = app_state.borrow_mut();
            let mut keep_adjusting = false;
            let playback = read_snapshot(&snapshot);
            let target = playback_is_seekable(&playback)
                .then(|| {
                    load_generation
                        .as_str()
                        .parse::<u64>()
                        .ok()
                        .and_then(|generation| {
                            seek_commit_target(
                                track_id,
                                generation,
                                playback.current_id,
                                playback.load_generation,
                            )
                        })
                })
                .flatten();
            let target = if position_ms.is_finite() && position_ms >= 0.0 {
                target
            } else {
                None
            };
            if let Some((track_id, load_generation)) = target {
                if committed {
                    state.pending_seek = None;
                    let request_id = next_control_request_id(&mut state);
                    let changed_at = Instant::now();
                    if submit_transport_command(
                        &mut state,
                        &sender,
                        &snapshot,
                        PlaybackCommand::SeekTo {
                            request_id,
                            track_id,
                            load_generation,
                            position_ms: position_ms.round() as i64,
                        },
                    ) {
                        state.pending_seek = Some(PendingSeek {
                            position_ms: position_ms.round() as i64,
                            track_id,
                            load_generation,
                            changed_at,
                            request_id: Some(request_id),
                            submitted_at: Some(changed_at),
                        });
                        keep_adjusting = true;
                    }
                } else {
                    state.pending_seek = Some(PendingSeek {
                        position_ms: position_ms.round() as i64,
                        track_id,
                        load_generation,
                        changed_at: Instant::now(),
                        request_id: None,
                        submitted_at: None,
                    });
                    keep_adjusting = true;
                }
            } else {
                state.pending_seek = None;
                state.feedback =
                    "Seek was canceled because its track is no longer current.".to_owned();
            }
            if let Some(ui) = weak.upgrade() {
                if committed || !keep_adjusting {
                    ui.set_adjusting_seek(keep_adjusting);
                }
                refresh_playback_ui(&ui, &mut state, &playback);
            }
        });
    }

    {
        let app_state = Rc::clone(app_state);
        let sender = sender.clone();
        let snapshot = Arc::clone(snapshot);
        let weak = ui.as_weak();
        ui.on_volume_changed(move |volume, committed| {
            let mut state = app_state.borrow_mut();
            let mut keep_adjusting = !committed;
            if volume.is_finite()
                && (MIN_VOLUME_PERCENT as f32..=MAX_VOLUME_PERCENT as f32).contains(&volume)
            {
                let volume = volume.round() as i32;
                if committed {
                    state.pending_volume = None;
                    state.volume = volume;
                    let request_id = next_control_request_id(&mut state);
                    let changed_at = Instant::now();
                    if submit_transport_command(
                        &mut state,
                        &sender,
                        &snapshot,
                        PlaybackCommand::SetVolume {
                            request_id,
                            volume_percent: volume,
                        },
                    ) {
                        state.pending_volume = Some(PendingValue {
                            value: volume,
                            changed_at,
                            request_id: Some(request_id),
                            submitted_at: Some(changed_at),
                        });
                        persist_library(&mut state);
                        keep_adjusting = true;
                    }
                } else {
                    state.pending_volume = Some(PendingValue {
                        value: volume,
                        changed_at: Instant::now(),
                        request_id: None,
                        submitted_at: None,
                    });
                }
            } else {
                keep_adjusting = false;
                state.pending_volume = None;
                state.feedback = "Volume must be between 0% and 100%.".to_owned();
            }
            if let Some(ui) = weak.upgrade() {
                if committed || !keep_adjusting {
                    ui.set_adjusting_volume(keep_adjusting);
                }
                refresh_playback_ui(&ui, &mut state, &read_snapshot(&snapshot));
            }
        });
    }

    {
        let app_state = Rc::clone(app_state);
        let sender = sender.clone();
        let snapshot = Arc::clone(snapshot);
        let weak = ui.as_weak();
        ui.on_playback_rate_changed(move |rate, committed| {
            let mut state = app_state.borrow_mut();
            let mut keep_adjusting = !committed;
            if rate.is_finite() && (MIN_PLAYBACK_RATE..=MAX_PLAYBACK_RATE).contains(&rate) {
                if committed {
                    state.pending_rate = None;
                    state.rate = rate;
                    let request_id = next_control_request_id(&mut state);
                    let changed_at = Instant::now();
                    if submit_transport_command(
                        &mut state,
                        &sender,
                        &snapshot,
                        PlaybackCommand::SetPlaybackRate { request_id, rate },
                    ) {
                        state.pending_rate = Some(PendingValue {
                            value: rate,
                            changed_at,
                            request_id: Some(request_id),
                            submitted_at: Some(changed_at),
                        });
                        persist_library(&mut state);
                        keep_adjusting = true;
                    }
                } else {
                    state.pending_rate = Some(PendingValue {
                        value: rate,
                        changed_at: Instant::now(),
                        request_id: None,
                        submitted_at: None,
                    });
                }
            } else {
                keep_adjusting = false;
                state.pending_rate = None;
                state.feedback = format!(
                    "Playback rate must be between {MIN_PLAYBACK_RATE}× and {MAX_PLAYBACK_RATE}×."
                );
            }
            if let Some(ui) = weak.upgrade() {
                if committed || !keep_adjusting {
                    ui.set_adjusting_rate(keep_adjusting);
                }
                refresh_playback_ui(&ui, &mut state, &read_snapshot(&snapshot));
            }
        });
    }
}

fn submit_transport_command(
    state: &mut AppState,
    sender: &SyncSender<PlaybackCommand>,
    snapshot: &Arc<Mutex<PlaybackSnapshot>>,
    command: PlaybackCommand,
) -> bool {
    match send_command(sender, command, snapshot) {
        Ok(()) => {
            state.feedback.clear();
            true
        }
        Err(message) => {
            state.feedback = message;
            false
        }
    }
}

fn next_control_request_id(state: &mut AppState) -> u64 {
    let request_id = state.next_control_request_id;
    state.next_control_request_id = state.next_control_request_id.wrapping_add(1).max(1);
    request_id
}

fn seek_commit_target(
    track_id: TrackId,
    load_generation: u64,
    current_track_id: Option<TrackId>,
    current_load_generation: u64,
) -> Option<(TrackId, u64)> {
    (track_id >= 0
        && current_track_id == Some(track_id)
        && load_generation == current_load_generation)
        .then_some((track_id, load_generation))
}

fn flush_pending_transport(
    ui: &AppWindow,
    state: &mut AppState,
    sender: &SyncSender<PlaybackCommand>,
    snapshot: &Arc<Mutex<PlaybackSnapshot>>,
) {
    let playback = read_snapshot(snapshot);
    if let Some(mut pending) = state.pending_seek.take() {
        if !pending_seek_matches_playback(&pending, &playback) {
            ui.set_adjusting_seek(false);
            if playback_is_seekable(&playback) {
                state.feedback =
                    "Seek was canceled because its track is no longer current.".to_owned();
            }
        } else if let (Some(request_id), Some(submitted_at)) =
            (pending.request_id, pending.submitted_at)
        {
            match control_resolution(request_id, submitted_at, playback.seek_ack, Instant::now()) {
                ControlResolution::Accepted => ui.set_adjusting_seek(false),
                ControlResolution::Rejected => ui.set_adjusting_seek(false),
                ControlResolution::TimedOut => {
                    ui.set_adjusting_seek(false);
                    state.feedback =
                        "VLC did not confirm the seek; showing the current playback position."
                            .to_owned();
                }
                ControlResolution::Waiting => state.pending_seek = Some(pending),
            }
        } else if pending.changed_at.elapsed() >= TRANSPORT_DEBOUNCE {
            let request_id = next_control_request_id(state);
            pending.request_id = Some(request_id);
            pending.submitted_at = Some(Instant::now());
            if submit_transport_command(
                state,
                sender,
                snapshot,
                PlaybackCommand::SeekTo {
                    request_id,
                    track_id: pending.track_id,
                    load_generation: pending.load_generation,
                    position_ms: pending.position_ms,
                },
            ) {
                state.pending_seek = Some(pending);
            } else {
                ui.set_adjusting_seek(false);
            }
        } else {
            state.pending_seek = Some(pending);
        }
    }

    if let Some(mut pending) = state.pending_volume.take() {
        if let (Some(request_id), Some(submitted_at)) = (pending.request_id, pending.submitted_at) {
            match control_resolution(
                request_id,
                submitted_at,
                playback.volume_ack,
                Instant::now(),
            ) {
                ControlResolution::Accepted | ControlResolution::Rejected => {
                    ui.set_adjusting_volume(false);
                }
                ControlResolution::TimedOut => {
                    ui.set_adjusting_volume(false);
                    state.feedback =
                        "VLC did not confirm the volume change; showing the accepted volume."
                            .to_owned();
                }
                ControlResolution::Waiting => state.pending_volume = Some(pending),
            }
        } else if pending.changed_at.elapsed() >= TRANSPORT_DEBOUNCE {
            let request_id = next_control_request_id(state);
            pending.request_id = Some(request_id);
            pending.submitted_at = Some(Instant::now());
            if submit_transport_command(
                state,
                sender,
                snapshot,
                PlaybackCommand::SetVolume {
                    request_id,
                    volume_percent: pending.value,
                },
            ) {
                state.pending_volume = Some(pending);
            } else {
                ui.set_adjusting_volume(false);
            }
        } else {
            state.pending_volume = Some(pending);
        }
    }

    if let Some(mut pending) = state.pending_rate.take() {
        if let (Some(request_id), Some(submitted_at)) = (pending.request_id, pending.submitted_at) {
            match control_resolution(
                request_id,
                submitted_at,
                playback.playback_rate_ack,
                Instant::now(),
            ) {
                ControlResolution::Accepted | ControlResolution::Rejected => {
                    ui.set_adjusting_rate(false);
                }
                ControlResolution::TimedOut => {
                    ui.set_adjusting_rate(false);
                    state.feedback =
                        "VLC did not confirm the playback-rate change; showing the accepted rate."
                            .to_owned();
                }
                ControlResolution::Waiting => state.pending_rate = Some(pending),
            }
        } else if pending.changed_at.elapsed() >= TRANSPORT_DEBOUNCE {
            let request_id = next_control_request_id(state);
            pending.request_id = Some(request_id);
            pending.submitted_at = Some(Instant::now());
            if submit_transport_command(
                state,
                sender,
                snapshot,
                PlaybackCommand::SetPlaybackRate {
                    request_id,
                    rate: pending.value,
                },
            ) {
                state.pending_rate = Some(pending);
            } else {
                ui.set_adjusting_rate(false);
            }
        } else {
            state.pending_rate = Some(pending);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ControlResolution {
    Waiting,
    Accepted,
    Rejected,
    TimedOut,
}

fn control_resolution(
    request_id: u64,
    submitted_at: Instant,
    acknowledgement: crate::playback::ControlAcknowledgement,
    now: Instant,
) -> ControlResolution {
    if acknowledgement.request_id == request_id {
        if acknowledgement.succeeded {
            ControlResolution::Accepted
        } else {
            ControlResolution::Rejected
        }
    } else if now.saturating_duration_since(submitted_at) >= TRANSPORT_ACK_TIMEOUT {
        ControlResolution::TimedOut
    } else {
        ControlResolution::Waiting
    }
}

#[derive(Clone)]
struct CardSource {
    id: TrackId,
    title: String,
    detail: String,
    genre: String,
    artwork: Option<PathBuf>,
    kind: &'static str,
    can_resume: bool,
    progress: String,
    saved: bool,
}

fn music_card_sources(state: &AppState) -> Vec<CardSource> {
    if state.music_section == "playlists" {
        let query = state.query.trim().to_lowercase();
        let mut cards: Vec<CardSource> = state
            .playlists
            .playlists()
            .iter()
            .filter_map(|playlist| {
                if !query.is_empty() && !playlist.name.to_lowercase().contains(&query) {
                    return None;
                }
                if state.music_genre != "All genres"
                    && !playlist
                        .track_ids
                        .iter()
                        .any(|id| genre_matches(&state.details, *id, &state.music_genre))
                {
                    return None;
                }
                let first = playlist
                    .track_ids
                    .first()
                    .and_then(|id| state.details.get(id));
                Some(CardSource {
                    id: playlist.id,
                    title: playlist.name.clone(),
                    detail: format!("{} tracks", playlist.track_ids.len()),
                    genre: first
                        .map(|details| details.genre.clone())
                        .unwrap_or_default(),
                    artwork: first.and_then(|details| details.artwork.clone()),
                    kind: "playlist",
                    can_resume: false,
                    progress: String::new(),
                    saved: false,
                })
            })
            .collect();
        sort_music_cards(&mut cards, &state.music_sort, &state.music_section);
        return cards;
    }
    let filtered_tracks = state
        .tracks
        .iter()
        .filter(|track| {
            state.music_genre == "All genres"
                || genre_matches(&state.details, track.id, &state.music_genre)
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut cards = browse::groups(
        &filtered_tracks,
        &state.details,
        &state.music_section,
        &state.query,
    )
    .into_iter()
    .map(|group: MusicGroup| {
        let details = state.details.get(&group.id);
        CardSource {
            id: group.id,
            title: group.title,
            detail: match state.music_section.as_str() {
                "albums" => group.detail,
                _ => format!("{} tracks", group.count),
            },
            genre: details
                .map(|details| details.genre.clone())
                .unwrap_or_default(),
            artwork: group.artwork,
            kind: "group",
            can_resume: false,
            progress: String::new(),
            saved: false,
        }
    })
    .collect::<Vec<_>>();
    sort_music_cards(&mut cards, &state.music_sort, &state.music_section);
    cards
}

fn sort_music_cards(cards: &mut [CardSource], sort: &str, section: &str) {
    cards.sort_by(|left, right| match sort {
        "title" => left
            .title
            .to_lowercase()
            .cmp(&right.title.to_lowercase())
            .then_with(|| left.id.cmp(&right.id)),
        "artist" => {
            let key = |card: &CardSource| {
                if section == "albums" {
                    card.detail.to_lowercase()
                } else {
                    card.title.to_lowercase()
                }
            };
            key(left)
                .cmp(&key(right))
                .then_with(|| left.title.to_lowercase().cmp(&right.title.to_lowercase()))
        }
        _ => right.id.cmp(&left.id),
    });
}

fn movie_card_sources(state: &mut AppState) -> Vec<CardSource> {
    state.movie_group_targets.clear();
    let filtered: Vec<&Track> = state
        .movies
        .iter()
        .filter(|movie| movie_matches(state, movie))
        .collect();
    let mut cards = Vec::new();
    if state.movie_level == "catalog" {
        let mut series: BTreeMap<String, Vec<&Track>> = BTreeMap::new();
        for movie in filtered {
            if let Some(episode) = episode_info(&movie.path) {
                series.entry(episode.series).or_default().push(movie);
            } else {
                cards.push(movie_card(
                    state,
                    movie,
                    "movie",
                    movie.title.clone(),
                    movie.format.clone(),
                ));
            }
        }
        for (name, episodes) in series {
            let representative = episodes
                .iter()
                .min_by_key(|movie| episode_info(&movie.path).map(|e| (e.season, e.episode)))
                .unwrap();
            state
                .movie_group_targets
                .insert(representative.id, MovieGroupTarget::Series(name.clone()));
            let details = state
                .details
                .get(&representative.id)
                .cloned()
                .unwrap_or_default();
            cards.push(CardSource {
                id: representative.id,
                title: name,
                detail: format!("{} episodes", episodes.len()),
                genre: details.genre,
                artwork: details.artwork,
                kind: "series",
                can_resume: false,
                progress: String::new(),
                saved: episodes
                    .iter()
                    .any(|movie| state.saved_movies.contains(&movie.id)),
            });
        }
    } else if state.movie_level == "seasons" {
        let series = state.selected_series.clone().unwrap_or_default();
        let mut seasons: BTreeMap<u32, Vec<&Track>> = BTreeMap::new();
        for movie in filtered {
            if let Some(episode) =
                episode_info(&movie.path).filter(|episode| episode.series == series)
            {
                seasons.entry(episode.season).or_default().push(movie);
            }
        }
        for (season, episodes) in seasons {
            let representative = episodes
                .iter()
                .min_by_key(|movie| episode_info(&movie.path).map(|e| e.episode))
                .unwrap();
            state.movie_group_targets.insert(
                representative.id,
                MovieGroupTarget::Season(series.clone(), season),
            );
            let details = state
                .details
                .get(&representative.id)
                .cloned()
                .unwrap_or_default();
            cards.push(CardSource {
                id: representative.id,
                title: format!("Season {season}"),
                detail: format!("{} episodes", episodes.len()),
                genre: details.genre,
                artwork: details.artwork,
                kind: "season",
                can_resume: false,
                progress: String::new(),
                saved: episodes
                    .iter()
                    .any(|movie| state.saved_movies.contains(&movie.id)),
            });
        }
    }
    sort_movie_cards(&mut cards, &state.movie_sort);
    cards
}

fn sort_movie_cards(cards: &mut [CardSource], sort: &str) {
    cards.sort_by(|left, right| {
        let title = || left.title.to_lowercase().cmp(&right.title.to_lowercase());
        match sort {
            "title" => title().then_with(|| left.id.cmp(&right.id)),
            "year-desc" => release_year(&right.title)
                .unwrap_or(0)
                .cmp(&release_year(&left.title).unwrap_or(0))
                .then_with(title),
            "year-asc" => release_year(&left.title)
                .unwrap_or(u16::MAX)
                .cmp(&release_year(&right.title).unwrap_or(u16::MAX))
                .then_with(title),
            _ => right.id.cmp(&left.id),
        }
    });
}

fn movie_card(
    state: &AppState,
    movie: &Track,
    kind: &'static str,
    title: String,
    detail: String,
) -> CardSource {
    let details = state.details.get(&movie.id);
    CardSource {
        id: movie.id,
        title,
        detail: if state.unavailable_media.contains(&movie.id) {
            format!("Unavailable · {detail}")
        } else {
            detail
        },
        genre: details
            .map(|details| details.genre.clone())
            .unwrap_or_default(),
        artwork: details.and_then(|details| details.artwork.clone()),
        kind,
        can_resume: can_resume(state.progress.get(&movie.id)),
        progress: progress_label(state.progress.get(&movie.id)),
        saved: state.saved_movies.contains(&movie.id),
    }
}

fn update_model<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    let common = model.row_count().min(rows.len());
    for (index, row) in rows.iter().take(common).enumerate() {
        if model.row_data(index).as_ref() != Some(row) {
            model.set_row_data(index, row.clone());
        }
    }
    while model.row_count() > rows.len() {
        model.remove(model.row_count() - 1);
    }
    for row in rows.into_iter().skip(common) {
        model.push(row);
    }
}

fn music_rows(
    state: &mut AppState,
    tracks: &[Track],
    offset: usize,
    playlist_positions: &[usize],
    playback: &PlaybackSnapshot,
) -> Vec<TrackRow> {
    tracks
        .iter()
        .enumerate()
        .map(|(local_index, track)| {
            let details = state.details.get(&track.id).cloned().unwrap_or_default();
            let artwork = cached_image(state, details.artwork.as_ref());
            let has_artwork = artwork.is_some();
            let playlist_position = playlist_positions
                .get(local_index)
                .copied()
                .map(|position| position.min(i32::MAX as usize) as i32)
                .unwrap_or(-1);
            let selected = match (
                state.music_section.as_str(),
                state.opened_playlist,
                playlist_position >= 0,
            ) {
                ("playlists", Some(playlist_id), true) => state
                    .playlist_selection
                    .contains(&(playlist_id, playlist_position as usize)),
                _ => state.selection.contains(track.id),
            };
            let detail = nonempty(&details.album_artist, &details.artist)
                .or_else(|| nonempty(&details.album, &track.format))
                .map(str::to_owned)
                .unwrap_or_else(|| "Local file".into());
            let duration = if playback.current_id == Some(track.id) && playback.duration_ms > 0 {
                format_time(playback.duration_ms)
            } else {
                "—".into()
            };
            let unavailable = state.unavailable_media.contains(&track.id);
            TrackRow {
                id: track.id,
                index: (offset + local_index) as i32,
                playlist_position,
                title: SharedString::from(track.title.as_str()),
                detail: SharedString::from(if unavailable {
                    format!("Unavailable · {detail}")
                } else {
                    detail
                }),
                genre: SharedString::from(nonempty(&details.genre, "—").unwrap_or("—")),
                duration: SharedString::from(duration),
                selected,
                unavailable,
                artwork: artwork.unwrap_or_default(),
                has_artwork,
                monogram: monogram(&track.title),
                can_resume: false,
                progress_label: SharedString::default(),
            }
        })
        .collect()
}

fn episode_rows(state: &mut AppState, tracks: &[Track], offset: usize) -> Vec<TrackRow> {
    tracks
        .iter()
        .enumerate()
        .map(|(local_index, movie)| {
            let details = state.details.get(&movie.id).cloned().unwrap_or_default();
            let artwork = cached_image(state, details.artwork.as_ref());
            let has_artwork = artwork.is_some();
            let info = episode_info(&movie.path);
            let detail = info
                .map(|e| format!("Season {} · Episode {}", e.season, e.episode))
                .unwrap_or_else(|| movie.format.clone());
            let unavailable = state.unavailable_media.contains(&movie.id);
            TrackRow {
                id: movie.id,
                index: (offset + local_index) as i32,
                playlist_position: -1,
                title: SharedString::from(movie.title.as_str()),
                detail: SharedString::from(if unavailable {
                    format!("Unavailable · {detail}")
                } else {
                    detail
                }),
                genre: SharedString::from(nonempty(&details.genre, "—").unwrap_or("—")),
                duration: SharedString::from("—"),
                selected: false,
                unavailable,
                artwork: artwork.unwrap_or_default(),
                has_artwork,
                monogram: monogram(&movie.title),
                can_resume: can_resume(state.progress.get(&movie.id)),
                progress_label: SharedString::from(progress_label(state.progress.get(&movie.id))),
            }
        })
        .collect()
}

fn browse_rows(state: &mut AppState, sources: Vec<CardSource>) -> Vec<BrowseRow> {
    sources
        .into_iter()
        .map(|source| {
            let artwork = cached_image(state, source.artwork.as_ref());
            let has_artwork = artwork.is_some();
            BrowseRow {
                id: source.id,
                title: SharedString::from(source.title.as_str()),
                detail: SharedString::from(source.detail),
                genre: SharedString::from(source.genre),
                year: SharedString::from(
                    release_year(&source.title)
                        .map(|year| year.to_string())
                        .unwrap_or_default(),
                ),
                artwork: artwork.unwrap_or_default(),
                has_artwork,
                monogram: monogram(&source.title),
                kind: SharedString::from(source.kind),
                can_resume: source.can_resume,
                progress_label: SharedString::from(source.progress),
                saved: source.saved,
            }
        })
        .collect()
}

fn refresh_ui(ui: &AppWindow, state: &mut AppState, playback: &PlaybackSnapshot) {
    update_model(
        &state.watched_roots_model,
        state
            .watched_roots
            .iter()
            .enumerate()
            .map(|(index, root)| WatchedRootRow {
                index: index.min(i32::MAX as usize) as i32,
                path: SharedString::from(root.display().to_string()),
                status: SharedString::from(
                    state
                        .root_scan_status
                        .get(&path_identity_key(root))
                        .map(String::as_str)
                        .unwrap_or("Watched"),
                ),
            })
            .collect(),
    );
    ui.set_watched_roots(ModelRc::from(Rc::clone(&state.watched_roots_model)));
    ui.set_media_controls_status(SharedString::from(state.media_controls_status.as_str()));
    ui.set_equalizer_enabled(state.equalizer_settings.enabled);
    update_model(
        &state.equalizer_preset_model,
        playback
            .equalizer_presets
            .iter()
            .map(|preset| SharedString::from(preset.as_str()))
            .collect(),
    );
    ui.set_equalizer_presets(ModelRc::from(Rc::clone(&state.equalizer_preset_model)));
    ui.set_equalizer_preset(SharedString::from(
        state
            .equalizer_settings
            .preset_index
            .and_then(|index| playback.equalizer_presets.get(index))
            .map(String::as_str)
            .unwrap_or("Custom settings"),
    ));
    ui.set_equalizer_preamp_db(state.equalizer_settings.preamp_db);
    ui.set_equalizer_preamp_label(SharedString::from(format!(
        "{:.1} dB",
        state.equalizer_settings.preamp_db
    )));
    update_model(
        &state.equalizer_band_model,
        playback
            .equalizer_band_frequencies_hz
            .iter()
            .take(state.equalizer_settings.bands_db.len())
            .enumerate()
            .map(|(index, frequency)| crate::EqualizerBandRow {
                index: index as i32,
                frequency: SharedString::from(format_frequency(*frequency)),
                gain: state.equalizer_settings.bands_db[index],
                gain_label: SharedString::from(format!(
                    "{:.1} dB",
                    state.equalizer_settings.bands_db[index]
                )),
            })
            .collect(),
    );
    ui.set_equalizer_bands(ModelRc::from(Rc::clone(&state.equalizer_band_model)));
    ui.set_ab_repeat_label(SharedString::from(ab_repeat_label(playback)));
    ui.set_can_set_ab_repeat(
        playback.current_id.is_some() && playback.duration_ms > 0 && playback.runtime_available,
    );
    ui.set_has_ab_repeat(playback.ab_repeat.a_ms.is_some() || playback.ab_repeat.b_ms.is_some());
    ui.set_sleep_timer_label(SharedString::from(sleep_timer_label(playback)));
    ui.set_cast_address_input(SharedString::from(state.cast_address_input.as_str()));
    ui.set_cast_target_label(SharedString::from(
        playback.cast_target.as_deref().unwrap_or("Local output"),
    ));
    ui.set_bookmark_name_input(SharedString::from(state.bookmark_name_input.as_str()));
    update_model(
        &state.bookmark_model,
        playback
            .current_id
            .into_iter()
            .flat_map(|media_id| {
                state
                    .bookmarks
                    .iter()
                    .enumerate()
                    .filter(move |(_, bookmark)| bookmark.media_id == media_id)
            })
            .map(|(index, bookmark)| crate::BookmarkRow {
                index: index.min(i32::MAX as usize) as i32,
                name: SharedString::from(bookmark.name.as_str()),
                time: SharedString::from(format_time(bookmark.position_ms)),
            })
            .collect(),
    );
    ui.set_bookmarks(ModelRc::from(Rc::clone(&state.bookmark_model)));
    ui.set_network_url_input(SharedString::from(state.network_url_input.as_str()));
    update_model(
        &state.network_history_model,
        sorted_network_history_rows(state),
    );
    ui.set_network_history(ModelRc::from(Rc::clone(&state.network_history_model)));
    update_model(
        &state.subtitle_result_model,
        state
            .subtitle_results
            .iter()
            .enumerate()
            .map(|(index, result)| crate::SubtitleResultRow {
                index: index.min(i32::MAX as usize) as i32,
                title: SharedString::from(if result.file_name.is_empty() {
                    result.release.as_str()
                } else {
                    result.file_name.as_str()
                }),
                detail: SharedString::from(format!(
                    "{}{}{}",
                    result.language,
                    if result.hearing_impaired {
                        " · hearing impaired"
                    } else {
                        ""
                    },
                    result
                        .download_count
                        .map(|count| format!(" · {count} downloads"))
                        .unwrap_or_default()
                )),
            })
            .collect(),
    );
    ui.set_subtitle_results(ModelRc::from(Rc::clone(&state.subtitle_result_model)));
    ui.set_subtitle_search_status(SharedString::from(state.subtitle_search_status.as_str()));
    ui.set_subtitle_language_input(SharedString::from(state.subtitle_language_input.as_str()));
    ui.set_opensubtitles_credentials_available(state.opensubtitles_credentials_available);
    ui.set_opensubtitles_status(SharedString::from(state.opensubtitles_status.as_str()));
    let music_detail = state.open_music_group.is_some() || state.opened_playlist.is_some();
    let music_list_visible =
        state.current_view == "music" && (state.music_section == "tracks" || music_detail);
    let full_music = if music_list_visible {
        music_collection(state)
    } else {
        Vec::new()
    };
    let music_pages = page_count(full_music.len(), MUSIC_PAGE_SIZE);
    let music_page = state.page_index.min(music_pages - 1);
    let music_start = music_page * MUSIC_PAGE_SIZE;
    let music_page_rows = full_music
        .iter()
        .skip(music_start)
        .take(MUSIC_PAGE_SIZE)
        .cloned()
        .collect::<Vec<_>>();
    let page_playlist_positions =
        if state.current_view == "music" && state.music_section == "playlists" {
            state
                .opened_playlist
                .map(|id| playlist_filtered_positions(state, id, &state.query))
                .unwrap_or_default()
                .into_iter()
                .skip(music_start)
                .take(MUSIC_PAGE_SIZE)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
    state.visible_tracks = if music_list_visible {
        music_page_rows.clone()
    } else {
        Vec::new()
    };
    state.visible_playlist_positions = if music_list_visible {
        page_playlist_positions.clone()
    } else {
        Vec::new()
    };
    let track_rows = music_rows(
        state,
        &music_page_rows,
        music_start,
        &page_playlist_positions,
        playback,
    );
    update_model(&state.track_model, track_rows);

    let music_card_visible =
        state.current_view == "music" && state.music_section != "tracks" && !music_detail;
    let music_sources = if music_card_visible {
        music_card_sources(state)
    } else {
        Vec::new()
    };
    let card_pages = page_count(music_sources.len(), CARD_PAGE_SIZE);
    let card_page = state.page_index.min(card_pages - 1);
    let card_start = card_page * CARD_PAGE_SIZE;
    let music_source_count = music_sources.len();
    let music_cards = music_sources
        .into_iter()
        .skip(card_start)
        .take(CARD_PAGE_SIZE)
        .collect();
    let music_browse_rows = browse_rows(state, music_cards);
    update_model(&state.music_card_model, music_browse_rows);

    let movie_card_visible = state.current_view == "movies" && state.movie_level != "episodes";
    let movie_sources = if movie_card_visible {
        movie_card_sources(state)
    } else {
        state.movie_group_targets.clear();
        Vec::new()
    };
    let movie_pages = page_count(movie_sources.len(), CARD_PAGE_SIZE);
    let movie_page = state.page_index.min(movie_pages - 1);
    let movie_start = movie_page * CARD_PAGE_SIZE;
    let movie_cards = movie_sources
        .into_iter()
        .skip(movie_start)
        .take(CARD_PAGE_SIZE)
        .collect();
    let movie_browse_rows = browse_rows(state, movie_cards);
    update_model(&state.movie_card_model, movie_browse_rows);

    let movie_list = if state.current_view == "movies" && state.movie_level == "episodes" {
        movie_collection(state)
    } else {
        Vec::new()
    };
    let episode_pages = page_count(movie_list.len(), MUSIC_PAGE_SIZE);
    let episode_page = state.page_index.min(episode_pages - 1);
    let episode_start = episode_page * MUSIC_PAGE_SIZE;
    let episode_page_rows = movie_list
        .iter()
        .skip(episode_start)
        .take(MUSIC_PAGE_SIZE)
        .cloned()
        .collect::<Vec<_>>();
    let episode_view_rows = episode_rows(state, &episode_page_rows, episode_start);
    update_model(&state.episode_model, episode_view_rows);

    let active_pages = if music_list_visible {
        music_pages
    } else if music_card_visible {
        card_pages
    } else if !movie_list.is_empty() {
        episode_pages
    } else {
        movie_pages
    };
    state.page_index = state.page_index.min(active_pages - 1);

    let selected_count = selected_displayed_tracks(state).len();
    let all_selected =
        !state.visible_tracks.is_empty() && selected_count == state.visible_tracks.len();
    let target_name = state
        .playlist_target
        .and_then(|id| state.playlists.playlists().iter().find(|p| p.id == id))
        .map(|p| p.name.clone())
        .unwrap_or_default();
    update_model(
        &state.playlist_targets_model,
        state
            .playlists
            .playlists()
            .iter()
            .map(|playlist| SharedString::from(playlist.name.as_str()))
            .collect(),
    );

    let movie_count = state
        .movies
        .iter()
        .filter(|movie| movie_matches(state, movie))
        .count();
    let selected_movie = state
        .selected_movie
        .and_then(|id| state.movies.iter().find(|movie| movie.id == id))
        .cloned();
    let selected_movie_details = selected_movie
        .as_ref()
        .and_then(|movie| state.details.get(&movie.id))
        .cloned()
        .unwrap_or_default();
    let selected_movie_art = cached_image(state, selected_movie_details.artwork.as_ref());
    let opened_playlist = state.opened_playlist.and_then(|id| {
        state
            .playlists
            .playlists()
            .iter()
            .find(|playlist| playlist.id == id)
    });
    let detail_title = if state.music_section == "playlists" {
        opened_playlist.map(|p| p.name.clone()).unwrap_or_default()
    } else {
        state
            .open_music_group
            .and_then(|id| music_group_title(state, id))
            .unwrap_or_default()
    };
    let target = artwork_target(state, playback);
    let target_track = target.and_then(|id| find_track(state, id));
    let target_art = target
        .and_then(|id| state.details.get(&id))
        .and_then(|details| details.artwork.as_ref());
    let page = if music_list_visible {
        music_page
    } else if music_card_visible {
        card_page
    } else if !movie_list.is_empty() {
        episode_page
    } else {
        movie_page
    };

    ui.set_current_view(SharedString::from(state.current_view.as_str()));
    ui.set_music_section(SharedString::from(state.music_section.as_str()));
    ui.set_music_detail_open(music_detail);
    ui.set_music_detail_title(SharedString::from(detail_title.as_str()));
    ui.set_tracks(ModelRc::from(Rc::clone(&state.track_model)));
    ui.set_movie_episodes(ModelRc::from(Rc::clone(&state.episode_model)));
    ui.set_music_cards(ModelRc::from(Rc::clone(&state.music_card_model)));
    ui.set_movie_cards(ModelRc::from(Rc::clone(&state.movie_card_model)));
    ui.set_playlist_targets(ModelRc::from(Rc::clone(&state.playlist_targets_model)));
    update_model(
        &state.movie_genres_model,
        available_genres(&state.movies, &state.details, &state.movie_genre),
    );
    ui.set_movie_genres(ModelRc::from(Rc::clone(&state.movie_genres_model)));
    update_model(
        &state.music_genres_model,
        available_genres(&state.tracks, &state.details, &state.music_genre),
    );
    ui.set_music_genres(ModelRc::from(Rc::clone(&state.music_genres_model)));
    ui.set_has_tracks(!state.tracks.is_empty());
    ui.set_has_movies(!state.movies.is_empty());
    ui.set_has_music_cards(music_card_visible && !state.music_card_model.row_count().eq(&0));
    ui.set_has_movie_cards(movie_card_visible && state.movie_card_model.row_count() > 0);
    ui.set_has_selection(selected_count > 0);
    ui.set_has_selected_playlist(state.playlist_target.is_some());
    ui.set_can_shuffle_opened_playlist(
        opened_playlist.is_some_and(|playlist| playlist.track_ids.len() > 1),
    );
    ui.set_selected_playlist_id(state.playlist_target.unwrap_or(-1));
    ui.set_selected_playlist_name(SharedString::from(target_name));
    ui.set_opened_playlist_name(SharedString::from(
        opened_playlist
            .map(|playlist| playlist.name.as_str())
            .unwrap_or_default(),
    ));
    ui.set_opened_playlist_id(opened_playlist.map(|playlist| playlist.id).unwrap_or(-1));
    ui.set_playlist_rename_input(SharedString::from(state.playlist_rename_input.as_str()));
    ui.set_select_all_checked(all_selected);
    ui.set_select_all_enabled(!state.visible_tracks.is_empty());
    ui.set_selected_count(selected_count.min(i32::MAX as usize) as i32);
    ui.set_visible_count(state.visible_tracks.len().min(i32::MAX as usize) as i32);
    ui.set_track_result_count(full_music.len().min(i32::MAX as usize) as i32);
    ui.set_search_query(SharedString::from(state.query.as_str()));
    ui.set_new_playlist_name(SharedString::from(state.new_playlist_name.as_str()));
    ui.set_library_status(SharedString::from(state.library_status.as_str()));
    ui.set_movie_level(SharedString::from(state.movie_level.as_str()));
    ui.set_movie_back_label(SharedString::from(match state.movie_level.as_str() {
        "episodes" => "Back to seasons",
        "seasons" => "Back to library",
        _ => "",
    }));
    ui.set_movie_filter(SharedString::from(state.movie_filter.as_str()));
    ui.set_movie_count(movie_count.min(i32::MAX as usize) as i32);
    ui.set_movie_view(SharedString::from(state.movie_view.as_str()));
    ui.set_movie_genre(SharedString::from(state.movie_genre.as_str()));
    ui.set_movie_sort(SharedString::from(match state.movie_sort.as_str() {
        "title" => "Title A–Z",
        "year-desc" => "Year newest",
        "year-asc" => "Year oldest",
        _ => "Recently added",
    }));
    ui.set_movie_library_count(state.movies.len().min(i32::MAX as usize) as i32);
    ui.set_saved_movie_count(state.saved_movies.len().min(i32::MAX as usize) as i32);
    ui.set_movie_results_heading(SharedString::from(if state.movie_view == "my-list" {
        "My List"
    } else {
        "All films"
    }));
    ui.set_movie_results_count(SharedString::from(format!("{} films", movie_count)));
    ui.set_movie_details_open(state.movie_details_open && selected_movie.is_some());
    ui.set_selected_movie_id(selected_movie.as_ref().map(|movie| movie.id).unwrap_or(-1));
    ui.set_selected_movie_title(SharedString::from(
        selected_movie
            .as_ref()
            .map(|movie| movie.title.as_str())
            .unwrap_or_default(),
    ));
    ui.set_selected_movie_progress(SharedString::from(
        selected_movie
            .as_ref()
            .map(|movie| progress_label(state.progress.get(&movie.id)))
            .unwrap_or_default(),
    ));
    ui.set_selected_movie_can_resume(
        selected_movie
            .as_ref()
            .is_some_and(|movie| can_resume(state.progress.get(&movie.id))),
    );
    ui.set_selected_movie_has_artwork(selected_movie_art.is_some());
    ui.set_selected_movie_artwork(selected_movie_art.unwrap_or_default());
    ui.set_selected_movie_genre(SharedString::from(
        nonempty(&selected_movie_details.genre, "—").unwrap_or("—"),
    ));
    ui.set_selected_movie_year(SharedString::from(
        selected_movie
            .as_ref()
            .and_then(|movie| release_year(&movie.title))
            .map(|year| year.to_string())
            .unwrap_or_else(|| "—".into()),
    ));
    ui.set_selected_movie_format(SharedString::from(
        selected_movie
            .as_ref()
            .map(|movie| movie.format.as_str())
            .unwrap_or("—"),
    ));
    ui.set_selected_movie_duration(SharedString::from(
        selected_movie
            .as_ref()
            .and_then(|movie| state.progress.get(&movie.id))
            .filter(|progress| progress.duration_ms > 0)
            .map(|progress| format_time(progress.duration_ms))
            .unwrap_or_else(|| "—".into()),
    ));
    ui.set_selected_movie_saved(
        selected_movie
            .as_ref()
            .is_some_and(|movie| state.saved_movies.contains(&movie.id)),
    );
    ui.set_music_genre(SharedString::from(state.music_genre.as_str()));
    ui.set_music_sort(SharedString::from(
        if state.music_section == "albums"
            && state.open_music_group.is_some()
            && !state.music_sort_explicit
        {
            "Disc / track"
        } else {
            match state.music_sort.as_str() {
                "album" => "Disc / track",
                "title" => "Title A–Z",
                "artist" => "Artist A–Z",
                _ => "Recently added",
            }
        },
    ));
    let result_count = if music_card_visible {
        music_source_count
    } else {
        full_music.len()
    };
    let result_kind = if music_detail || state.music_section == "tracks" {
        "tracks"
    } else {
        match state.music_section.as_str() {
            "artists" => "artists",
            "genres" => "genres",
            "playlists" => "playlists",
            _ => "albums",
        }
    };
    ui.set_music_results_heading(SharedString::from(if music_detail {
        if detail_title.is_empty() {
            "Tracks"
        } else {
            detail_title.as_str()
        }
    } else {
        match state.music_section.as_str() {
            "artists" => "Artists",
            "tracks" => "Tracks",
            "genres" => "Genres",
            "playlists" => "Playlists",
            _ => "Albums",
        }
    }));
    ui.set_music_results_count(SharedString::from(format!("{result_count} {result_kind}")));
    ui.set_artwork_target_title(SharedString::from(
        target_track
            .map(|track| track.title.as_str())
            .unwrap_or_default(),
    ));
    ui.set_artwork_target_ready(target_track.is_some());
    ui.set_artwork_target_missing(
        target_track.is_some() && target_art.is_none_or(|path| !path.is_file()),
    );
    ui.set_tmdb_token_input(SharedString::from(state.tmdb_token_input.as_str()));
    ui.set_tmdb_token_available(state.tmdb_token_available);
    ui.set_settings_status(SharedString::from(state.settings_status.as_str()));
    ui.set_settings_error(state.settings_error);
    ui.set_audio_delay_input(SharedString::from(state.audio_delay_input.as_str()));
    ui.set_subtitle_delay_input(SharedString::from(state.subtitle_delay_input.as_str()));
    ui.set_jump_time_input(SharedString::from(state.jump_time_input.as_str()));
    ui.set_video_fill(state.video_fill);
    ui.set_page_index(page.min(i32::MAX as usize) as i32);
    ui.set_page_count(active_pages.min(i32::MAX as usize) as i32);
    refresh_playback_ui(ui, state, playback);
}

fn page_count(items: usize, page_size: usize) -> usize {
    items.div_ceil(page_size).max(1)
}

fn music_group_title(state: &AppState, id: TrackId) -> Option<String> {
    let tracks = state
        .tracks
        .iter()
        .filter(|track| {
            state.music_genre == "All genres"
                || genre_matches(&state.details, track.id, &state.music_genre)
        })
        .cloned()
        .collect::<Vec<_>>();
    browse::groups(&tracks, &state.details, &state.music_section, &state.query)
        .into_iter()
        .find(|group| group.id == id)
        .map(|group| group.title)
}

fn refresh_playback_ui(ui: &AppWindow, state: &mut AppState, playback: &PlaybackSnapshot) {
    ui.set_runtime_ready(playback.runtime_checked);
    ui.set_runtime_available(playback.runtime_available);
    ui.set_runtime_message(SharedString::from(playback.runtime_message.as_str()));
    ui.set_has_current_track(playback.current_id.is_some());
    ui.set_playing_video(playback.current_is_video);
    ui.set_can_return_to_video(can_return_to_video(playback));
    ui.set_fullscreen_controls_visible(fullscreen_controls_visible(
        playback,
        state.fullscreen_activity_at,
        Instant::now(),
    ));
    ui.set_current_title(SharedString::from(if playback.current_title.is_empty() {
        "No track selected"
    } else {
        playback.current_title.as_str()
    }));
    ui.set_current_load_generation(SharedString::from(playback.load_generation.to_string()));
    let source_track = playback.current_id.and_then(|id| find_track(state, id));
    ui.set_source_title(SharedString::from(
        source_track
            .map(|track| track.title.as_str())
            .unwrap_or_default(),
    ));
    ui.set_source_path(SharedString::from(
        source_track
            .map(|track| match &track.source {
                TrackSource::Local => track.path.display().to_string(),
                TrackSource::Network(url) => url.display_label(),
            })
            .unwrap_or_default(),
    ));
    ui.set_source_kind(SharedString::from(
        match source_track.map(|track| &track.source) {
            Some(TrackSource::Network(_)) => "Network stream",
            _ => "Local file",
        },
    ));
    ui.set_source_format(SharedString::from(
        source_track
            .map(|track| track.format.as_str())
            .unwrap_or_default(),
    ));
    ui.set_player_state(SharedString::from(playback.state.label()));
    ui.set_player_time(SharedString::from(format_time(playback.position_ms)));
    ui.set_player_duration(SharedString::from(format_time(playback.duration_ms)));
    ui.set_player_duration_ms(playback.duration_ms.max(1) as f32);
    ui.set_current_track_id(playback.current_id.unwrap_or(-1));
    ui.set_player_volume_label(SharedString::from(format!("{}%", playback.volume_percent)));
    ui.set_playback_rate_label(SharedString::from(format!(
        "{:.2}×",
        playback.playback_rate
    )));
    ui.set_queue_position(SharedString::from(queue_position_label(
        playback.queue_index,
        playback.queue_len,
    )));
    ui.set_repeat_mode(SharedString::from(match playback.repeat_mode {
        RepeatMode::Off => "Off",
        RepeatMode::One => "One",
        RepeatMode::All => "All",
    }));
    ui.set_playback_shuffle(playback.shuffle);
    ui.set_queue_revision(playback.queue_revision.min(i32::MAX as u64) as i32);
    if state.rendered_queue_revision != playback.queue_revision {
        let revision = playback.queue_revision.to_string();
        update_model(
            &state.queue_model,
            playback
                .queue_titles
                .iter()
                .enumerate()
                .map(|(position, title)| QueueRow {
                    position: position.min(i32::MAX as usize) as i32,
                    revision: SharedString::from(revision.as_str()),
                    title: SharedString::from(title.as_str()),
                })
                .collect(),
        );
        state.rendered_queue_revision = playback.queue_revision;
    }
    if refresh_delay_drafts(state, playback) {
        state.subtitle_results.clear();
        state.subtitle_search_pending = None;
        state.subtitle_search_status = if state.subtitle_download_pending.is_some() {
            "Media changed. A pending download will be cached but not auto-attached.".into()
        } else {
            String::new()
        };
        update_model(&state.subtitle_result_model, Vec::new());
        ui.set_subtitle_results(ModelRc::from(Rc::clone(&state.subtitle_result_model)));
        ui.set_subtitle_search_status(SharedString::from(state.subtitle_search_status.as_str()));
        ui.set_audio_delay_input(SharedString::from(state.audio_delay_input.as_str()));
        ui.set_subtitle_delay_input(SharedString::from(state.subtitle_delay_input.as_str()));
    }
    refresh_track_choice_models(state, playback);
    ui.set_audio_tracks(ModelRc::from(Rc::clone(&state.audio_model)));
    ui.set_subtitle_tracks(ModelRc::from(Rc::clone(&state.subtitle_model)));
    ui.set_chapters(ModelRc::from(Rc::clone(&state.chapter_model)));
    ui.set_can_toggle_pause(can_toggle_pause(playback));
    ui.set_can_stop(playback.runtime_available && playback.current_id.is_some());
    let can_seek = playback_is_seekable(playback);
    ui.set_can_seek(can_seek);
    if !can_seek {
        state.pending_seek = None;
        ui.set_adjusting_seek(false);
    }
    ui.set_can_adjust_playback(playback.runtime_available);
    ui.set_is_paused(playback.state == PlaybackUiState::Paused);
    if !ui.get_adjusting_seek() {
        ui.set_player_position_ms(playback.position_ms.max(0) as f32);
    }
    if !ui.get_adjusting_volume() {
        ui.set_player_volume(playback.volume_percent as f32);
        state.volume = playback.volume_percent;
    }
    if !ui.get_adjusting_rate() {
        ui.set_playback_rate(playback.playback_rate);
        state.rate = playback.playback_rate;
    }
    ui.set_status_message(SharedString::from(status_message(
        state.feedback.as_str(),
        playback,
    )));
}

fn refresh_delay_drafts(state: &mut AppState, playback: &PlaybackSnapshot) -> bool {
    let current_changed = state.last_current_id != playback.current_id
        || state.last_current_load_generation != playback.load_generation;
    if current_changed {
        state.last_current_id = playback.current_id;
        state.last_current_load_generation = playback.load_generation;
        state.audio_delay_input = format!("{}", playback.audio_delay_us / 1000);
        state.subtitle_delay_input = format!("{}", playback.subtitle_delay_us / 1000);
    }
    current_changed
}

fn status_message<'a>(feedback: &'a str, playback: &'a PlaybackSnapshot) -> &'a str {
    if matches!(
        playback.state,
        PlaybackUiState::Error | PlaybackUiState::Unavailable
    ) && !playback.message.is_empty()
    {
        playback.message.as_str()
    } else if !feedback.is_empty() {
        feedback
    } else {
        playback.message.as_str()
    }
}

fn playback_is_seekable(playback: &PlaybackSnapshot) -> bool {
    playback.runtime_available
        && playback.current_id.is_some()
        && playback.duration_ms > 0
        && matches!(
            playback.state,
            PlaybackUiState::Loading | PlaybackUiState::Playing | PlaybackUiState::Paused
        )
}

fn pending_seek_matches_playback(pending: &PendingSeek, playback: &PlaybackSnapshot) -> bool {
    playback_is_seekable(playback)
        && playback.current_id == Some(pending.track_id)
        && playback.load_generation == pending.load_generation
}

fn queue_position_label(queue_index: usize, queue_len: usize) -> String {
    if queue_len == 0 {
        "No queue".to_owned()
    } else {
        format!("{queue_index} of {queue_len} in queue")
    }
}

fn can_toggle_pause(playback: &PlaybackSnapshot) -> bool {
    playback.runtime_available
        && playback.current_id.is_some()
        && matches!(
            playback.state,
            PlaybackUiState::Idle
                | PlaybackUiState::Loading
                | PlaybackUiState::Playing
                | PlaybackUiState::Paused
                | PlaybackUiState::Ended
        )
}

fn can_return_to_video(playback: &PlaybackSnapshot) -> bool {
    playback.current_id.is_some()
        && playback.current_is_video
        && matches!(
            playback.state,
            PlaybackUiState::Loading
                | PlaybackUiState::Playing
                | PlaybackUiState::Paused
                | PlaybackUiState::Ended
                | PlaybackUiState::Error
        )
}

fn fullscreen_controls_visible(
    playback: &PlaybackSnapshot,
    activity_at: Option<Instant>,
    now: Instant,
) -> bool {
    playback.state != PlaybackUiState::Playing
        || activity_at
            .is_none_or(|at| now.saturating_duration_since(at) < FULLSCREEN_CONTROLS_TIMEOUT)
}

fn refresh_track_choice_models(state: &AppState, playback: &PlaybackSnapshot) {
    update_model(
        &state.audio_model,
        playback
            .audio_tracks
            .iter()
            .map(|track| TrackChoice {
                id: track.id,
                title: SharedString::from(track.name.as_str()),
            })
            .collect(),
    );
    update_model(
        &state.subtitle_model,
        playback
            .subtitle_tracks
            .iter()
            .map(|track| TrackChoice {
                id: track.id,
                title: SharedString::from(track.name.as_str()),
            })
            .collect(),
    );
    update_model(
        &state.chapter_model,
        playback
            .chapters
            .iter()
            .map(|track| TrackChoice {
                id: track.id,
                title: SharedString::from(track.name.as_str()),
            })
            .collect(),
    );
}

fn refresh_video_frame(
    ui: &AppWindow,
    frames: &FrameMailbox,
    displayed_generation: &Cell<u64>,
    playback: &PlaybackSnapshot,
    picture_in_picture: &Rc<RefCell<Option<PictureInPictureWindow>>>,
    displayed_frame: &Rc<RefCell<Option<DisplayedVideoFrame>>>,
) {
    let generation = frames.generation();
    if displayed_generation.get() != generation {
        displayed_generation.set(generation);
        ui.set_video_frame(Image::default());
        ui.set_has_video_frame(false);
        displayed_frame.borrow_mut().take();
        if let Some(pip) = picture_in_picture.borrow().as_ref() {
            pip.set_video_frame(Image::default());
            pip.set_has_video_frame(false);
        }
    }

    if !playback.current_is_video {
        if ui.get_has_video_frame() {
            ui.set_video_frame(Image::default());
            ui.set_has_video_frame(false);
        }
        displayed_frame.borrow_mut().take();
        if let Some(pip) = picture_in_picture.borrow().as_ref() {
            let _ = pip.hide();
            pip.set_video_frame(Image::default());
            pip.set_has_video_frame(false);
        }
        ui.set_picture_in_picture_open(false);
        return;
    }

    if let Some(frame) = frames.take_latest() {
        if let Some(pixels) = frame.packed_rgba() {
            let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
                &pixels,
                frame.width,
                frame.height,
            );
            ui.set_video_frame(Image::from_rgba8(buffer));
            ui.set_has_video_frame(true);
            *displayed_frame.borrow_mut() = Some(DisplayedVideoFrame {
                width: frame.width,
                height: frame.height,
                rgba: pixels.into(),
            });
            if let Some(pip) = picture_in_picture.borrow().as_ref() {
                pip.set_video_frame(ui.get_video_frame());
                pip.set_has_video_frame(true);
            }
        }
    }

    if let Some(pip) = picture_in_picture.borrow().as_ref() {
        pip.set_current_title(SharedString::from(playback.current_title.as_str()));
        pip.set_player_state(SharedString::from(playback.state.label()));
        pip.set_can_toggle_pause(can_toggle_pause(playback));
        pip.set_is_paused(playback.state == PlaybackUiState::Paused);
    }
}

fn read_snapshot(snapshot: &Mutex<PlaybackSnapshot>) -> PlaybackSnapshot {
    snapshot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

fn format_time(milliseconds: i64) -> String {
    let seconds = milliseconds.max(0) / 1000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn format_frequency(frequency_hz: f32) -> String {
    if !frequency_hz.is_finite() || frequency_hz <= 0.0 {
        "Frequency unavailable".into()
    } else if frequency_hz >= 1000.0 {
        format!("{:.1} kHz", frequency_hz / 1000.0)
    } else {
        format!("{frequency_hz:.0} Hz")
    }
}

fn ab_repeat_label(playback: &PlaybackSnapshot) -> String {
    match (playback.ab_repeat.a_ms, playback.ab_repeat.b_ms) {
        (Some(a_ms), Some(b_ms)) if b_ms > a_ms => {
            format!("A {} · B {}", format_time(a_ms), format_time(b_ms))
        }
        (Some(a_ms), Some(_)) => {
            format!("A {} · B needs to follow A", format_time(a_ms))
        }
        (Some(a_ms), None) => format!("A {} · Set B", format_time(a_ms)),
        (None, Some(b_ms)) => format!("B {} · Set A", format_time(b_ms)),
        (None, None) => "A/B loop off".into(),
    }
}

fn sleep_timer_label(playback: &PlaybackSnapshot) -> String {
    playback
        .sleep_timer_remaining_ms
        .map(|remaining| format!("Sleep timer · {} remaining", format_time(remaining as i64)))
        .unwrap_or_else(|| "Sleep timer off".into())
}

fn sorted_network_history_rows(state: &AppState) -> Vec<crate::NetworkHistoryRow> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default();
    let mut history = state.network_history.iter().enumerate().collect::<Vec<_>>();
    history.sort_by_key(|(_, entry)| std::cmp::Reverse(entry.last_played_at));
    history
        .into_iter()
        .map(|(index, entry)| crate::NetworkHistoryRow {
            index: index.min(i32::MAX as usize) as i32,
            title: SharedString::from(entry.title.as_str()),
            address: SharedString::from(entry.url.display_label()),
            played: SharedString::from(relative_time(now, entry.last_played_at)),
        })
        .collect()
}

fn relative_time(now: i64, then: i64) -> String {
    let age = now.saturating_sub(then).max(0);
    if age < 60 {
        "Just now".into()
    } else if age < 3600 {
        format!("{} min ago", age / 60)
    } else if age < 86_400 {
        format!("{} hr ago", age / 3600)
    } else {
        format!("{} d ago", age / 86_400)
    }
}

fn movie_from_path(id: TrackId, path: std::path::PathBuf) -> Option<Track> {
    if !path.is_file() || !is_movie_path(&path) {
        return None;
    }

    Some(Track {
        source: TrackSource::Local,
        id,
        title: readable_filename(&path),
        path,
        format: MOVIE_FORMAT.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        add_paths, add_watched_root, apply_artwork_result, apply_automatic_artwork_result,
        apply_metadata_result, apply_scan_result, bookmark_name, can_resume, can_toggle_pause,
        collection_start_index, collection_starts_movie, collection_track_at, control_resolution,
        flush_pending_metadata, import_m3u8_into_state, library_snapshot,
        media_control_pause_change_needed, movie_double_click, movie_from_path, music_collection,
        next_artwork_intent, path_identity_key, pending_seek_matches_playback, play_collection,
        playlist_collection, playlist_filtered_positions, playlist_move_target,
        playlist_owner_for_queue, playlist_row_is_current, queue_automatic_artwork_retry,
        queue_position_label, reconcile_equalizer_settings, refresh_delay_drafts,
        refresh_track_choice_models, remove_watched_root, restore_queue_tracks,
        schedule_automatic_artwork, seek_commit_target, selected_displayed_tracks,
        set_movie_artwork_target, sort_music_tracks, status_message, update_network_history,
        update_queue_checkpoint, wide_prefix_eq_ascii, windows_path_identity_units, AppState,
        ControlResolution, PendingSeek, MOVIE_DETAILS_CLICK_DELAY, TRANSPORT_ACK_TIMEOUT,
    };
    use crate::metadata::{ArtworkKind, ArtworkRequest, LocalMetadata};
    use crate::model::{MediaDetails, Track, TrackSource};
    use crate::playback::queue::{QueueCheckpoint, RepeatMode};
    use crate::playback::{
        is_movie, ControlAcknowledgement, EqualizerSettings, PlaybackCommand, PlaybackSnapshot,
        PlaybackTrack, PlaybackUiState,
    };
    use crate::playlists::{Playlist, PlaylistCollection};
    use crate::storage::{LibraryStore, Progress};
    use slint::{Model, ModelRc, SharedString, VecModel};
    use std::cell::RefCell;
    use std::collections::{HashMap, VecDeque};
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::{Duration, Instant};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(windows)]
    fn windows_verbatim_path(path: &Path) -> PathBuf {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};

        let normal =
            windows_path_identity_units(&path.as_os_str().encode_wide().collect::<Vec<_>>());
        let mut verbatim = [b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16].to_vec();
        verbatim.extend(normal);
        PathBuf::from(std::ffi::OsString::from_wide(&verbatim))
    }

    #[test]
    fn windows_path_identity_helper_normalizes_drive_and_unc_prefixes_without_case_folding() {
        let wide = |value: &str| value.encode_utf16().collect::<Vec<_>>();
        assert_eq!(
            windows_path_identity_units(&wide(r"\\?\C:\Media\Movie.mp4")),
            wide(r"C:\Media\Movie.mp4")
        );
        assert_eq!(
            windows_path_identity_units(&wide(r"\\?\UNC\Server\Share\Media\Movie.mp4")),
            wide(r"\\Server\Share\Media\Movie.mp4")
        );
        assert!(wide_prefix_eq_ascii(&wide("unc\\server"), b"UNC\\"));
        let volume = wide(r"\\?\Volume{01234567-89AB-CDEF-0123-456789ABCDEF}\Media");
        assert_eq!(windows_path_identity_units(&volume), volume);
    }

    #[cfg(windows)]
    #[test]
    fn path_identity_matches_verbatim_drive_paths_and_keeps_component_case() {
        let ordinary = Path::new(r"C:\Library\Album\Track.flac");
        let verbatim = Path::new(r"\\?\C:\Library\Album\Track.flac");
        assert_eq!(path_identity_key(ordinary), path_identity_key(verbatim));
        assert_eq!(
            path_identity_key(Path::new(r"\\Server\Share\Album\Track.flac")),
            path_identity_key(Path::new(r"\\?\UNC\Server\Share\Album\Track.flac"))
        );
        assert_ne!(
            path_identity_key(ordinary),
            path_identity_key(Path::new(r"C:\library\Album\Track.flac"))
        );
    }

    #[test]
    fn media_control_play_pause_requests_are_idempotent_and_resume_restored_queue() {
        let playing = PlaybackSnapshot {
            current_id: Some(4),
            queue_len: 1,
            state: PlaybackUiState::Playing,
            ..PlaybackSnapshot::default()
        };
        assert!(media_control_pause_change_needed(&playing, true));
        assert!(media_control_pause_change_needed(&playing, false));

        let paused = PlaybackSnapshot {
            state: PlaybackUiState::Paused,
            ..playing.clone()
        };
        assert!(media_control_pause_change_needed(&paused, true));
        assert!(media_control_pause_change_needed(&paused, false));

        let restored = PlaybackSnapshot {
            current_id: None,
            queue_len: 1,
            queue_restored: true,
            state: PlaybackUiState::Idle,
            ..PlaybackSnapshot::default()
        };
        assert!(media_control_pause_change_needed(&restored, false));
        assert!(media_control_pause_change_needed(&restored, true));
    }

    #[test]
    fn bookmark_names_match_storage_byte_and_control_limits() {
        let max_name = "é".repeat(64);
        assert_eq!(bookmark_name(&max_name, 0).unwrap().len(), 128);
        assert!(bookmark_name(&"é".repeat(65), 0).is_err());
        assert!(bookmark_name("name\n", 0).is_err());
        assert!(bookmark_name("\tname", 0).is_err());
        assert!(bookmark_name("   ", 0).unwrap().starts_with("Point at "));
    }

    #[test]
    fn network_history_sanitizes_title_and_reopens_without_changing_signed_url() {
        let url = format!(
            "https://radio.example.test/{}/https%3A%2F%2F%0A?token=private",
            "long-segment".repeat(40),
        );
        let track = Track::from_network_url(41, &url).unwrap();
        let actual_url = match &track.source {
            TrackSource::Network(url) => url.clone(),
            TrackSource::Local => unreachable!(),
        };
        let mut state = AppState {
            movies: vec![track],
            ..AppState::default()
        };
        let playback = PlaybackSnapshot {
            current_id: Some(41),
            load_generation: 1,
            state: PlaybackUiState::Loading,
            ..PlaybackSnapshot::default()
        };
        assert!(update_network_history(&mut state, &playback));
        assert_eq!(state.network_history[0].title, "Network stream");
        assert!(state.network_history[0].title.len() <= 256);
        assert!(!state.network_history[0].title.chars().any(char::is_control));

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("rust-vlc-network-history-{unique}.sqlite3"));
        let mut store = LibraryStore::open(&path).unwrap();
        store.save(&library_snapshot(&state)).unwrap();
        drop(store);
        let store = LibraryStore::open(&path).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.network_history[0].title, "Network stream");
        assert_eq!(loaded.network_history[0].url.as_str(), actual_url.as_str());
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn imported_m3u8_mixed_media_is_visible_and_playable_in_playlist_order() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("rust-vlc-m3u8-mixed-{unique}"));
        std::fs::create_dir_all(&directory).expect("create M3U8 fixture directory");
        let audio_path = directory.join("song.mp3");
        let video_path = directory.join("movie.mkv");
        let playlist_path = directory.join("mixed.m3u8");
        std::fs::write(&audio_path, b"audio fixture").expect("write audio fixture");
        std::fs::write(&video_path, b"video fixture").expect("write video fixture");
        std::fs::write(
            &playlist_path,
            b"#EXTM3U\nsong.mp3\nmovie.mkv\nhttps://radio.example.test/live\nmovie.mkv\n",
        )
        .expect("write M3U8 fixture");
        let mut state = AppState {
            tracks: vec![Track::from_path(1, audio_path).unwrap()],
            movies: vec![movie_from_path(2, video_path).unwrap()],
            next_id: 3,
            ..AppState::default()
        };

        import_m3u8_into_state(&mut state, &playlist_path).unwrap();
        let playlist_id = state.opened_playlist.unwrap();
        let displayed = playlist_collection(&state, playlist_id, "");
        let ids = displayed.iter().map(|track| track.id).collect::<Vec<_>>();
        assert_eq!(ids, [1, 2, 3, 2]);
        assert!(!collection_starts_movie(&displayed, 0));
        assert!(collection_starts_movie(&displayed, 3));
        assert_eq!(
            playlist_filtered_positions(&state, playlist_id, ""),
            [0, 1, 2, 3]
        );
        state.visible_tracks = displayed.clone();
        state.visible_playlist_positions = vec![0, 1, 2, 3];
        assert!(playlist_row_is_current(&state, playlist_id, 3, 3, 2));
        assert_eq!(music_collection(&state), displayed);

        let queue = music_collection(&state);
        let (sender, receiver) = mpsc::sync_channel(1);
        let snapshot = Arc::new(Mutex::new(PlaybackSnapshot::default()));
        assert!(play_collection(
            &mut state, &sender, &snapshot, queue, 3, None
        ));
        match receiver.try_recv().unwrap() {
            PlaybackCommand::PlayQueue {
                tracks,
                start_index,
                ..
            } => {
                assert_eq!(start_index, 3);
                assert_eq!(tracks.iter().map(|track| track.id).collect::<Vec<_>>(), ids);
                assert_eq!(tracks[2].format, "STREAM");
            }
            _ => panic!("playlist row should submit the displayed collection queue"),
        }
        std::fs::remove_dir_all(directory).expect("remove M3U8 fixture directory");
    }

    #[test]
    fn repeated_model_bindings_keep_identity_after_incremental_row_updates() {
        let model = Rc::new(VecModel::<SharedString>::default());
        let initial = ModelRc::from(Rc::clone(&model));
        let repeated = ModelRc::from(Rc::clone(&model));
        assert_eq!(initial, repeated);

        super::update_model(&model, vec![SharedString::from("One")]);
        let updated = ModelRc::from(Rc::clone(&model));
        assert_eq!(initial, updated);
        assert_eq!(updated.row_count(), 1);
    }

    #[test]
    fn delay_drafts_survive_same_load_snapshot_refresh() {
        let mut state = AppState {
            last_current_id: Some(7),
            last_current_load_generation: 3,
            audio_delay_input: "125".into(),
            subtitle_delay_input: "-250".into(),
            ..AppState::default()
        };
        let playback = PlaybackSnapshot {
            current_id: Some(7),
            load_generation: 3,
            audio_delay_us: 0,
            subtitle_delay_us: 0,
            ..PlaybackSnapshot::default()
        };

        assert!(!refresh_delay_drafts(&mut state, &playback));
        assert_eq!(state.audio_delay_input, "125");
        assert_eq!(state.subtitle_delay_input, "-250");
    }

    #[test]
    fn startup_snapshot_default_does_not_replace_saved_equalizer_settings() {
        let saved = EqualizerSettings {
            enabled: true,
            preamp_db: 3.5,
            bands_db: [1.0, 2.0, 3.0, 4.0, 5.0, 4.0, 3.0, 2.0, 1.0, 0.0],
            preset_index: None,
        };
        let mut state = AppState {
            equalizer_settings: saved,
            equalizer_settings_pending: true,
            ..AppState::default()
        };
        let playback = PlaybackSnapshot::default();

        assert!(!reconcile_equalizer_settings(&mut state, &playback));
        assert_eq!(state.equalizer_settings, saved);
        assert_eq!(library_snapshot(&state).equalizer_settings, saved);
    }

    #[test]
    fn return_to_video_only_restores_fullscreen_for_current_video() {
        let ui = crate::AppWindow::new().expect("create test window");
        let checkpoint = QueueCheckpoint {
            entry_ids: vec![3, 4],
            order: vec![0, 1],
            position: Some(0),
            repeat_mode: RepeatMode::All,
            shuffle: true,
            position_ms: 12_000,
            ..QueueCheckpoint::default()
        };
        let snapshot = Arc::new(Mutex::new(PlaybackSnapshot {
            current_id: Some(3),
            load_generation: 8,
            current_is_video: true,
            current_title: "Movie".into(),
            state: PlaybackUiState::Paused,
            queue_index: 1,
            queue_len: 2,
            queue_titles: vec!["Movie".into(), "Next".into()],
            queue_revision: 7,
            queue_checkpoint: checkpoint.clone(),
            queue_restored: true,
            position_ms: 12_000,
            duration_ms: 60_000,
            audio_tracks: vec![PlaybackTrack {
                id: 5,
                name: "English".into(),
            }],
            subtitle_tracks: vec![PlaybackTrack {
                id: 6,
                name: "English subtitles".into(),
            }],
            audio_track: 5,
            subtitle_track: 6,
            audio_delay_us: 125_000,
            subtitle_delay_us: -250_000,
            ..PlaybackSnapshot::default()
        }));
        let state = Rc::new(RefCell::new(AppState::default()));
        let (sender, receiver) = mpsc::sync_channel(1);

        super::wire_library_ui(&ui, &state, &sender, &snapshot);
        ui.invoke_return_to_video();

        assert!(ui.get_movie_fullscreen());
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        let playback = snapshot.lock().unwrap();
        assert_eq!(playback.load_generation, 8);
        assert_eq!(playback.queue_checkpoint, checkpoint);
        assert_eq!(playback.position_ms, 12_000);
        assert_eq!(playback.audio_track, 5);
        assert_eq!(playback.subtitle_track, 6);
        assert_eq!(playback.audio_delay_us, 125_000);
        assert_eq!(playback.subtitle_delay_us, -250_000);
    }

    #[test]
    fn fullscreen_controls_hide_only_after_playing_inactivity_timeout() {
        let now = Instant::now();
        let playing = PlaybackSnapshot {
            state: PlaybackUiState::Playing,
            ..PlaybackSnapshot::default()
        };
        let inactive_since = Some(now - super::FULLSCREEN_CONTROLS_TIMEOUT);

        assert!(super::fullscreen_controls_visible(&playing, None, now));
        assert!(super::fullscreen_controls_visible(
            &playing,
            Some(now - Duration::from_millis(500)),
            now,
        ));
        assert!(!super::fullscreen_controls_visible(
            &playing,
            inactive_since,
            now,
        ));

        for state in [PlaybackUiState::Paused, PlaybackUiState::Error] {
            let playback = PlaybackSnapshot {
                state,
                ..PlaybackSnapshot::default()
            };
            assert!(super::fullscreen_controls_visible(
                &playback,
                inactive_since,
                now,
            ));
        }
    }

    #[test]
    fn startup_snapshot_does_not_replace_saved_queue_before_restore() {
        let checkpoint = QueueCheckpoint {
            entry_ids: vec![4, 4, 9],
            order: vec![2, 0, 1],
            position: Some(1),
            wrapped_from_end: false,
            repeat_mode: RepeatMode::All,
            shuffle: true,
            position_ms: 12_345,
        };
        let mut state = AppState {
            queue_checkpoint: checkpoint.clone(),
            ..AppState::default()
        };

        assert!(!update_queue_checkpoint(
            &mut state,
            &PlaybackSnapshot::default(),
            true,
        ));
        assert_eq!(state.queue_checkpoint, checkpoint);
        assert_eq!(library_snapshot(&state).queue_checkpoint, checkpoint);
    }

    #[test]
    fn restored_queue_keeps_duplicate_track_occurrences() {
        let track = Track {
            source: TrackSource::Local,
            id: 4,
            path: PathBuf::from("track.mp3"),
            title: "Track".into(),
            format: "MP3".into(),
        };
        let state = AppState {
            tracks: vec![track],
            ..AppState::default()
        };
        let checkpoint = QueueCheckpoint {
            entry_ids: vec![4, 4],
            order: vec![1, 0],
            position: Some(0),
            ..QueueCheckpoint::default()
        };

        assert_eq!(
            restore_queue_tracks(&state, &checkpoint)
                .iter()
                .map(|track| track.id)
                .collect::<Vec<_>>(),
            [4, 4]
        );
    }

    #[test]
    fn watched_overlapping_roots_do_not_duplicate_library_entries() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rust-vlc-roots-{unique}"));
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).expect("create nested root");
        let audio = root.join("audio.mp3");
        let video = nested.join("video.mkv");
        std::fs::write(&audio, b"audio fixture").expect("write audio fixture");
        std::fs::write(&video, b"video fixture").expect("write video fixture");
        let root = root.canonicalize().expect("canonicalize root");
        let nested = nested.canonicalize().expect("canonicalize nested root");
        let mut state = AppState::default();

        assert!(add_watched_root(&mut state, root.clone()).unwrap());
        assert!(!add_watched_root(&mut state, root.clone()).unwrap());
        assert!(add_watched_root(&mut state, nested.clone()).unwrap());
        assert_eq!(state.pending_root_scans.len(), 2);

        let parent_scan = crate::scan::scan_directory(&root);
        apply_scan_result(&mut state, root.clone(), parent_scan);
        let nested_scan = crate::scan::scan_directory(&nested);
        apply_scan_result(&mut state, nested, nested_scan);

        assert_eq!(state.tracks.len(), 1);
        assert_eq!(state.movies.len(), 1);
        assert!(audio.is_file());
        assert!(video.is_file());
        std::fs::remove_dir_all(root).expect("remove root fixture");
    }

    #[test]
    fn removing_watched_root_keeps_library_history_playlist_and_source_media() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rust-vlc-remove-root-{unique}"));
        std::fs::create_dir_all(&root).expect("create root fixture");
        let path = root.join("song.mp3");
        std::fs::write(&path, b"audio fixture").expect("write audio fixture");
        let track = Track {
            source: TrackSource::Local,
            id: 1,
            path: path.clone(),
            title: "Song".into(),
            format: "MP3".into(),
        };
        let mut state = AppState {
            tracks: vec![track],
            watched_roots: vec![root.clone()],
            pending_root_scans: VecDeque::from([root.clone()]),
            ..AppState::default()
        };
        state.progress.insert(
            1,
            Progress {
                media_id: 1,
                position_ms: 1_000,
                duration_ms: 3_000,
                completed: false,
            },
        );
        state.playlists = PlaylistCollection::restore(
            vec![Playlist {
                id: 7,
                name: "Saved".into(),
                track_ids: vec![1],
            }],
            Some(7),
        );

        assert_eq!(remove_watched_root(&mut state, 0), Some(root.clone()));
        assert!(state.watched_roots.is_empty());
        assert!(state.pending_root_scans.is_empty());
        assert_eq!(state.tracks.len(), 1);
        assert_eq!(state.progress[&1].position_ms, 1_000);
        assert_eq!(state.playlists.playlists()[0].track_ids, [1]);
        assert!(path.is_file());
        std::fs::remove_dir_all(root).expect("remove root fixture");
    }

    #[test]
    fn incomplete_root_scan_keeps_entries_but_complete_scan_marks_missing_media() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rust-vlc-offline-root-{unique}"));
        std::fs::create_dir_all(&root).expect("create root fixture");
        let path = root.join("missing.mp4");
        let mut state = AppState {
            movies: vec![Track {
                source: TrackSource::Local,
                id: 2,
                path,
                title: "Missing".into(),
                format: "MOVIE".into(),
            }],
            watched_roots: vec![root.clone()],
            ..AppState::default()
        };
        state.progress.insert(
            2,
            Progress {
                media_id: 2,
                position_ms: 7_000,
                duration_ms: 20_000,
                completed: false,
            },
        );

        apply_scan_result(
            &mut state,
            root.clone(),
            crate::scan::ScanOutcome {
                warnings: vec!["The root is temporarily unavailable.".into()],
                complete: false,
                ..crate::scan::ScanOutcome::default()
            },
        );
        assert!(!state.unavailable_media.contains(&2));
        assert_eq!(state.progress[&2].position_ms, 7_000);

        apply_scan_result(
            &mut state,
            root.clone(),
            crate::scan::ScanOutcome {
                complete: true,
                ..crate::scan::ScanOutcome::default()
            },
        );
        assert!(state.unavailable_media.contains(&2));
        assert_eq!(state.movies.len(), 1);
        assert_eq!(state.progress[&2].position_ms, 7_000);
        std::fs::remove_dir_all(root).expect("remove root fixture");
    }

    #[test]
    fn watched_scan_requeues_metadata_when_an_existing_file_changes() {
        let root = std::env::temp_dir().join(format!(
            "rust-vlc-fingerprint-refresh-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create root fixture");
        let path = root.join("movie.mp4");
        std::fs::write(&path, b"changed movie bytes").expect("write fixture");
        #[cfg(windows)]
        let scan_root = windows_verbatim_path(&root);
        #[cfg(not(windows))]
        let scan_root = root.clone();
        #[cfg(windows)]
        let scan_path = windows_verbatim_path(&path);
        #[cfg(not(windows))]
        let scan_path = path.clone();
        let mut state = AppState {
            movies: vec![Track {
                source: TrackSource::Local,
                id: 41,
                path: path.clone(),
                title: "Movie".into(),
                format: "MP4".into(),
            }],
            watched_roots: vec![root.clone()],
            ..AppState::default()
        };
        assert_eq!(path_identity_key(&path), path_identity_key(&scan_path));
        state.file_fingerprints.insert(
            path_identity_key(&path),
            crate::scan::FileFingerprint {
                path: path.clone(),
                size: 5,
                modified: Some(UNIX_EPOCH),
            },
        );
        assert!(!add_watched_root(&mut state, scan_root.clone()).unwrap());
        assert_eq!(state.watched_roots.len(), 1);

        apply_scan_result(
            &mut state,
            scan_root,
            crate::scan::ScanOutcome {
                paths: vec![scan_path.clone()],
                fingerprints: vec![crate::scan::FileFingerprint {
                    path: scan_path,
                    size: 19,
                    modified: None,
                }],
                complete: true,
                ..crate::scan::ScanOutcome::default()
            },
        );

        assert_eq!(state.pending_metadata, VecDeque::from([41]));
        assert_eq!(state.movies.len(), 1);
        assert!(!state.unavailable_media.contains(&41));
        assert_eq!(
            state
                .root_scan_status
                .get(&path_identity_key(&root))
                .map(String::as_str),
            Some("Scanned · 1 media files")
        );
        std::fs::remove_dir_all(root).expect("remove root fixture");
    }

    #[test]
    fn movie_resume_position_travels_with_the_queue_command() {
        let movie = Track {
            source: TrackSource::Local,
            id: 1,
            path: PathBuf::from("movie.mp4"),
            title: "Movie".into(),
            format: "MP4".into(),
        };
        let mut state = AppState::default();
        state.movies.push(movie.clone());
        let snapshot = Arc::new(Mutex::new(PlaybackSnapshot {
            current_id: Some(movie.id),
            load_generation: 9,
            duration_ms: 30_000,
            position_ms: 20_000,
            state: PlaybackUiState::Playing,
            ..PlaybackSnapshot::default()
        }));
        let (sender, receiver) = mpsc::sync_channel(1);

        assert!(play_collection(
            &mut state,
            &sender,
            &snapshot,
            vec![movie],
            0,
            Some(20_000),
        ));
        let PlaybackCommand::PlayQueue {
            tracks,
            start_index,
            resume_position_ms,
        } = receiver.try_recv().expect("play queue command")
        else {
            panic!("expected a PlayQueue command");
        };
        assert_eq!(tracks[0].id, 1);
        assert_eq!(start_index, 0);
        assert_eq!(resume_position_ms, Some(20_000));
    }

    #[test]
    fn import_metadata_stays_pending_until_the_bounded_worker_can_accept_it() {
        let root = std::env::temp_dir().join(format!(
            "rust-vlc-metadata-retry-{}-{}",
            std::process::id(),
            fastrand::u64(..)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let media = root.join("Track.mp3");
        std::fs::write(&media, b"fixture").unwrap();

        let mut state = AppState::default();
        let (added, available) = add_paths(&mut state, vec![media], Some(false));
        assert_eq!(added, 1);
        assert!(!available);
        assert_eq!(state.pending_metadata, [1]);

        state.library_worker =
            Some(crate::library_worker::LibraryWorker::spawn(root.join("cache")).unwrap());
        assert!(flush_pending_metadata(&mut state));
        assert!(state.pending_metadata.is_empty());

        let started = Instant::now();
        let metadata_arrived = loop {
            let events = state.library_worker.as_ref().unwrap().drain_events();
            if events
                .iter()
                .any(|event| matches!(event, crate::library_worker::LibraryEvent::Metadata(1, _)))
            {
                break true;
            }
            if started.elapsed() >= Duration::from_secs(2) {
                break false;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(
            metadata_arrived,
            "the retried metadata job reaches the worker"
        );
        state.library_worker.take();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pending_slider_value_waits_for_matching_ack_or_times_out() {
        let submitted_at = Instant::now();
        let request_id = 42;

        assert_eq!(
            control_resolution(
                request_id,
                submitted_at,
                ControlAcknowledgement {
                    request_id: 41,
                    succeeded: true,
                },
                submitted_at + Duration::from_millis(50),
            ),
            ControlResolution::Waiting,
            "an older snapshot must not overwrite the submitted slider value"
        );
        assert_eq!(
            control_resolution(
                request_id,
                submitted_at,
                ControlAcknowledgement {
                    request_id,
                    succeeded: true,
                },
                submitted_at + Duration::from_millis(50),
            ),
            ControlResolution::Accepted
        );
        assert_eq!(
            control_resolution(
                request_id,
                submitted_at,
                ControlAcknowledgement {
                    request_id,
                    succeeded: false,
                },
                submitted_at + Duration::from_millis(50),
            ),
            ControlResolution::Rejected
        );
        assert_eq!(
            control_resolution(
                request_id,
                submitted_at,
                ControlAcknowledgement::default(),
                submitted_at + TRANSPORT_ACK_TIMEOUT,
            ),
            ControlResolution::TimedOut
        );
    }

    #[test]
    fn seek_release_requires_the_same_track_and_load_generation() {
        assert_eq!(seek_commit_target(17, 4, Some(18), 4), None);
        assert_eq!(
            seek_commit_target(18, 4, Some(18), 5),
            None,
            "a duplicate occurrence with the same media ID must reject a stale gesture"
        );
        assert_eq!(seek_commit_target(-1, 4, Some(18), 4), None);
        assert_eq!(seek_commit_target(18, 5, Some(18), 5), Some((18, 5)));
    }

    #[test]
    fn stopped_or_ended_playback_rejects_a_pending_seek_for_the_same_load() {
        let pending = PendingSeek {
            position_ms: 9_000,
            track_id: 18,
            load_generation: 5,
            changed_at: Instant::now(),
            request_id: None,
            submitted_at: None,
        };
        let seekable = PlaybackSnapshot {
            runtime_available: true,
            current_id: Some(18),
            load_generation: 5,
            duration_ms: 30_000,
            state: PlaybackUiState::Playing,
            ..PlaybackSnapshot::default()
        };

        assert!(pending_seek_matches_playback(&pending, &seekable));
        for state in [PlaybackUiState::Idle, PlaybackUiState::Ended] {
            let stopped = PlaybackSnapshot {
                state,
                ..seekable.clone()
            };
            assert!(!pending_seek_matches_playback(&pending, &stopped));
        }
    }

    #[test]
    fn playback_errors_take_precedence_over_routine_feedback() {
        for state in [PlaybackUiState::Error, PlaybackUiState::Unavailable] {
            let playback = PlaybackSnapshot {
                state,
                message: "VLC could not open this file".into(),
                ..PlaybackSnapshot::default()
            };
            assert_eq!(
                status_message("Playlist saved", &playback),
                "VLC could not open this file"
            );
        }

        let playback = PlaybackSnapshot {
            state: PlaybackUiState::Playing,
            message: "Playing".into(),
            ..PlaybackSnapshot::default()
        };
        assert_eq!(
            status_message("Playlist saved", &playback),
            "Playlist saved"
        );
        assert_eq!(status_message("", &playback), "Playing");
    }

    #[test]
    fn playback_choice_models_update_when_tracks_arrive_for_the_same_load() {
        let state = AppState::default();
        let mut playback = PlaybackSnapshot {
            current_id: Some(17),
            load_generation: 4,
            ..PlaybackSnapshot::default()
        };
        refresh_track_choice_models(&state, &playback);
        assert_eq!(state.audio_model.row_count(), 0);
        assert_eq!(state.subtitle_model.row_count(), 0);
        assert_eq!(state.chapter_model.row_count(), 0);

        playback.audio_tracks.push(PlaybackTrack {
            id: 1,
            name: "English".into(),
        });
        playback.subtitle_tracks.push(PlaybackTrack {
            id: 2,
            name: "English subtitles".into(),
        });
        playback.chapters.push(PlaybackTrack {
            id: 3,
            name: "Chapter 1".into(),
        });
        refresh_track_choice_models(&state, &playback);

        assert_eq!(state.audio_model.row_count(), 1);
        assert_eq!(state.subtitle_model.row_count(), 1);
        assert_eq!(state.chapter_model.row_count(), 1);
        assert_eq!(state.audio_model.row_data(0).unwrap().title, "English");
        assert_eq!(
            state.subtitle_model.row_data(0).unwrap().title,
            "English subtitles"
        );
        assert_eq!(state.chapter_model.row_data(0).unwrap().title, "Chapter 1");
    }

    #[test]
    fn queue_position_label_uses_the_published_one_based_index() {
        assert_eq!(queue_position_label(1, 2), "1 of 2 in queue");
        assert_eq!(queue_position_label(2, 2), "2 of 2 in queue");
        assert_eq!(queue_position_label(0, 0), "No queue");
    }

    #[test]
    fn ended_current_track_keeps_replay_enabled() {
        let playback = PlaybackSnapshot {
            runtime_available: true,
            current_id: Some(17),
            state: PlaybackUiState::Ended,
            ..PlaybackSnapshot::default()
        };
        assert!(can_toggle_pause(&playback));
        assert!(!can_toggle_pause(&PlaybackSnapshot {
            runtime_available: false,
            ..playback
        }));
    }

    #[test]
    fn play_next_requires_the_current_displayed_collection_row() {
        let first = Track {
            source: TrackSource::Local,
            id: 1,
            path: PathBuf::from("first.mp3"),
            title: "First".into(),
            format: "MP3".into(),
        };
        let second = Track {
            source: TrackSource::Local,
            id: 2,
            path: PathBuf::from("second.mp3"),
            title: "Second".into(),
            format: "MP3".into(),
        };
        let collection = [first, second];
        assert_eq!(
            collection_track_at(&collection, 2, 1).map(|track| track.id),
            Some(2)
        );
        assert!(collection_track_at(&collection, 2, 0).is_none());
        assert!(collection_track_at(&collection, 2, -1).is_none());
    }

    #[test]
    fn artwork_result_after_local_retry_is_ignored() {
        let mut state = AppState::default();
        let online_generation = next_artwork_intent(&mut state, 5);
        next_artwork_intent(&mut state, 5); // Retry local artwork.

        assert!(!apply_artwork_result(
            &mut state,
            5,
            online_generation,
            Ok(Some(PathBuf::from("stale-online.jpg"))),
        ));
        assert_eq!(state.details.get(&5), None);
        assert!(state.feedback.is_empty());
    }

    #[test]
    fn older_online_artwork_result_cannot_replace_a_newer_request() {
        let mut state = AppState::default();
        let older_generation = next_artwork_intent(&mut state, 5);
        let newer_generation = next_artwork_intent(&mut state, 5);

        assert!(!apply_artwork_result(
            &mut state,
            5,
            older_generation,
            Ok(Some(PathBuf::from("older-online.jpg"))),
        ));
        assert!(apply_artwork_result(
            &mut state,
            5,
            newer_generation,
            Ok(Some(PathBuf::from("newer-online.jpg"))),
        ));
        let details = state.details.get(&5).unwrap();
        assert_eq!(details.artwork, Some(PathBuf::from("newer-online.jpg")));
        assert!(details.artwork_override);
    }

    #[test]
    fn movie_artwork_target_must_be_a_current_movie() {
        let current_movie = Track {
            source: TrackSource::Local,
            id: 4,
            path: PathBuf::from("current.mkv"),
            title: "Current".into(),
            format: "MOVIE".into(),
        };
        let mut state = AppState {
            movies: vec![current_movie],
            artwork_target: Some(99),
            ..AppState::default()
        };

        assert!(set_movie_artwork_target(&mut state, 4));
        assert_eq!(state.artwork_target, Some(4));
        assert!(!set_movie_artwork_target(&mut state, 99));
        assert_eq!(state.artwork_target, None);
    }

    #[test]
    fn local_movie_tracks_require_existing_video_files_and_keep_movie_identity() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("rust-vlc-movie-test-{unique}"));
        std::fs::create_dir_all(&directory).expect("create test directory");
        let video_path = directory.join("A_Test.Movie.MKV");
        let text_path = directory.join("not-a-video.txt");
        std::fs::write(&video_path, b"test video placeholder").expect("write test video");
        std::fs::write(&text_path, b"not video").expect("write test text file");

        let movie = movie_from_path(73, video_path.clone()).expect("supported local movie");
        assert_eq!(movie.id, 73);
        assert_eq!(movie.title, "A Test Movie");
        assert!(is_movie(&movie));
        assert!(movie_from_path(74, text_path).is_none());
        assert!(movie_from_path(75, directory.join("missing.mp4")).is_none());

        std::fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn displayed_playlist_selects_duplicate_occurrences_independently_and_keeps_play_order() {
        let tracks = vec![
            Track {
                source: TrackSource::Local,
                id: 1,
                path: PathBuf::from("one.mp3"),
                title: "One".into(),
                format: "MP3".into(),
            },
            Track {
                source: TrackSource::Local,
                id: 2,
                path: PathBuf::from("two.mp3"),
                title: "Two".into(),
                format: "MP3".into(),
            },
        ];
        let mut state = AppState {
            tracks: tracks.clone(),
            current_view: "music".into(),
            music_section: "playlists".into(),
            opened_playlist: Some(7),
            ..AppState::default()
        };
        state.playlists = PlaylistCollection::restore(
            vec![Playlist {
                id: 7,
                name: "Mix".into(),
                track_ids: vec![1, 2, 1],
            }],
            Some(7),
        );
        let collection = music_collection(&state);
        assert_eq!(
            collection.iter().map(|track| track.id).collect::<Vec<_>>(),
            [1, 2, 1]
        );
        assert_eq!(collection_start_index(&collection, 1, 2), Some(2));
        state.visible_tracks = collection;
        state.visible_playlist_positions = vec![0, 1, 2];
        state.playlist_selection.insert((7, 2));
        assert_eq!(
            selected_displayed_tracks(&state)
                .iter()
                .map(|track| track.id)
                .collect::<Vec<_>>(),
            [1],
        );
        state.playlist_selection.insert((7, 0));
        assert_eq!(
            selected_displayed_tracks(&state)
                .iter()
                .map(|track| track.id)
                .collect::<Vec<_>>(),
            [1, 1],
        );
    }

    #[test]
    fn filtered_duplicate_playlist_rows_keep_underlying_positions_for_edits() {
        let tracks = vec![
            Track {
                source: TrackSource::Local,
                id: 1,
                path: PathBuf::from("one.mp3"),
                title: "One".into(),
                format: "MP3".into(),
            },
            Track {
                source: TrackSource::Local,
                id: 2,
                path: PathBuf::from("two.mp3"),
                title: "Two".into(),
                format: "MP3".into(),
            },
        ];
        let mut state = AppState {
            tracks,
            current_view: "music".into(),
            music_section: "playlists".into(),
            opened_playlist: Some(7),
            query: "one".into(),
            ..AppState::default()
        };
        state.playlists = PlaylistCollection::restore(
            vec![Playlist {
                id: 7,
                name: "Mix".into(),
                track_ids: vec![1, 2, 1],
            }],
            Some(7),
        );
        let positions = playlist_filtered_positions(&state, 7, &state.query);
        assert_eq!(positions, [0, 2]);
        state.visible_tracks = playlist_collection(&state, 7, &state.query);
        state.visible_playlist_positions = positions.clone();

        assert!(playlist_row_is_current(&state, 7, 0, 0, 1));
        assert!(playlist_row_is_current(&state, 7, 1, 2, 1));
        assert!(!playlist_row_is_current(&state, 7, 1, 0, 1));
        assert_eq!(playlist_move_target(&positions, 0, 1), Some(2));
    }

    #[test]
    fn filtered_playlist_rows_on_later_pages_keep_their_occurrence_positions() {
        let one = Track {
            source: TrackSource::Local,
            id: 1,
            path: PathBuf::from("one.mp3"),
            title: "One".into(),
            format: "MP3".into(),
        };
        let two = Track {
            source: TrackSource::Local,
            id: 2,
            path: PathBuf::from("two.mp3"),
            title: "Two".into(),
            format: "MP3".into(),
        };
        let mut playlist_entries = vec![1; 50];
        playlist_entries.insert(25, 2);
        playlist_entries.extend([1, 2, 1]);

        let mut state = AppState {
            tracks: vec![one.clone(), two],
            current_view: "music".into(),
            music_section: "playlists".into(),
            opened_playlist: Some(7),
            query: "one".into(),
            page_index: 1,
            ..AppState::default()
        };
        state.playlists = PlaylistCollection::restore(
            vec![Playlist {
                id: 7,
                name: "Mix".into(),
                track_ids: playlist_entries,
            }],
            Some(7),
        );

        let positions = playlist_filtered_positions(&state, 7, &state.query);
        assert_eq!(positions.len(), 52);
        assert_eq!(&positions[50..], [51, 53]);
        state.visible_tracks = vec![one.clone(), one];
        state.visible_playlist_positions = positions[50..].to_vec();

        assert!(playlist_row_is_current(&state, 7, 50, 51, 1));
        assert!(playlist_row_is_current(&state, 7, 51, 53, 1));
        assert_eq!(playlist_move_target(&positions, 50, 1), Some(53));
    }

    #[test]
    fn active_playlist_owner_requires_an_exact_displayed_collection() {
        let tracks = vec![
            Track {
                source: TrackSource::Local,
                id: 1,
                path: PathBuf::from("one.mp3"),
                title: "One".into(),
                format: "MP3".into(),
            },
            Track {
                source: TrackSource::Local,
                id: 2,
                path: PathBuf::from("two.mp3"),
                title: "Two".into(),
                format: "MP3".into(),
            },
        ];
        let mut state = AppState {
            tracks: tracks.clone(),
            current_view: "music".into(),
            music_section: "playlists".into(),
            opened_playlist: Some(7),
            query: "one".into(),
            ..AppState::default()
        };
        state.playlists = PlaylistCollection::restore(
            vec![Playlist {
                id: 7,
                name: "Mix".into(),
                track_ids: vec![1, 2, 1],
            }],
            Some(7),
        );

        let displayed = playlist_collection(&state, 7, &state.query);
        let owner = playlist_owner_for_queue(&state, &displayed).expect("exact playlist queue");
        assert_eq!(owner.playlist_id, 7);
        assert_eq!(owner.query, "one");
        assert!(playlist_owner_for_queue(&state, &tracks).is_none());
    }

    #[test]
    fn movie_resume_excludes_short_and_completed_progress() {
        let progress = |position_ms, completed| Progress {
            media_id: 3,
            position_ms,
            duration_ms: 100_000,
            completed,
        };
        assert!(!can_resume(Some(&progress(10_000, false))));
        assert!(can_resume(Some(&progress(10_001, false))));
        assert!(!can_resume(Some(&progress(95_000, false))));
        assert!(!can_resume(Some(&progress(40_000, true))));
        assert!(!can_resume(None));
    }

    #[test]
    fn movie_double_click_requires_the_same_title_within_the_detail_delay() {
        let now = Instant::now();
        assert!(movie_double_click(
            Some((3, now)),
            3,
            now + Duration::from_millis(100)
        ));
        assert!(!movie_double_click(
            Some((2, now)),
            3,
            now + Duration::from_millis(100)
        ));
        assert!(!movie_double_click(
            Some((3, now)),
            3,
            now + MOVIE_DETAILS_CLICK_DELAY + Duration::from_millis(1)
        ));
    }

    #[test]
    fn background_metadata_does_not_replace_a_manual_artwork_override() {
        let mut state = AppState::default();
        state.details.insert(
            1,
            MediaDetails {
                artist: "Old artist".into(),
                artwork: Some(PathBuf::from("manual-cover.jpg")),
                artwork_override: true,
                ..MediaDetails::default()
            },
        );
        apply_metadata_result(
            &mut state,
            1,
            LocalMetadata {
                artist: Some("Tagged artist".into()),
                artwork: Some(PathBuf::from("embedded-cover.jpg")),
                ..LocalMetadata::default()
            },
        );
        let details = state.details.get(&1).unwrap();
        assert_eq!(details.artist, "Tagged artist");
        assert_eq!(details.artwork, Some(PathBuf::from("manual-cover.jpg")));
        assert!(details.artwork_override);
    }

    #[test]
    fn automatic_music_artwork_requires_matching_current_tags_and_no_local_art() {
        let track = Track {
            source: TrackSource::Local,
            id: 5,
            path: PathBuf::from("song.mp3"),
            title: "Song title".into(),
            format: "MP3".into(),
        };
        let request = ArtworkRequest {
            title: "Song title".into(),
            artist: Some("Artist".into()),
            album: Some("Album".into()),
            kind: ArtworkKind::Music,
        };
        let mut state = AppState {
            tracks: vec![track],
            ..AppState::default()
        };
        state.details.insert(
            5,
            MediaDetails {
                artist: "Artist".into(),
                album: "Album".into(),
                ..MediaDetails::default()
            },
        );

        assert!(apply_automatic_artwork_result(
            &mut state,
            5,
            &request,
            Ok(Some(PathBuf::from("cache/automatic-cover.jpg"))),
        ));
        let details = state.details.get(&5).unwrap();
        assert_eq!(
            details.artwork,
            Some(PathBuf::from("cache/automatic-cover.jpg"))
        );
        assert!(!details.artwork_override);

        state.details.get_mut(&5).unwrap().artwork = None;
        let stale_request = ArtworkRequest {
            title: "Old title".into(),
            ..request.clone()
        };
        assert!(!apply_automatic_artwork_result(
            &mut state,
            5,
            &stale_request,
            Ok(Some(PathBuf::from("cache/stale-cover.jpg"))),
        ));
        assert!(!apply_automatic_artwork_result(
            &mut state,
            5,
            &request,
            Ok(None)
        ));
        assert!(!apply_automatic_artwork_result(
            &mut state,
            5,
            &request,
            Err("provider failed".into()),
        ));

        state.details.get_mut(&5).unwrap().artwork = Some(PathBuf::from("nearby.jpg"));
        assert!(!apply_automatic_artwork_result(
            &mut state,
            5,
            &request,
            Ok(Some(PathBuf::from("cache/remote-cover.jpg"))),
        ));
        assert_eq!(
            state.details.get(&5).unwrap().artwork,
            Some(PathBuf::from("nearby.jpg"))
        );
    }

    #[test]
    fn retried_music_artwork_waits_for_metadata_without_blocking_later_tracks() {
        let mut state = AppState {
            tracks: vec![
                Track {
                    source: TrackSource::Local,
                    id: 1,
                    path: PathBuf::from("first.mp3"),
                    title: "First".into(),
                    format: "MP3".into(),
                },
                Track {
                    source: TrackSource::Local,
                    id: 2,
                    path: PathBuf::from("second.mp3"),
                    title: "Second".into(),
                    format: "MP3".into(),
                },
            ],
            automatic_artwork_cursor: 1,
            ..AppState::default()
        };
        state.automatic_artwork_retries.push_back(1);
        state.automatic_artwork_readiness.insert(2, false);

        schedule_automatic_artwork(&mut state);
        assert_eq!(state.automatic_artwork_retries, [1]);
        assert_eq!(state.automatic_artwork_cursor, 1);

        state.automatic_artwork_readiness.insert(1, false);
        schedule_automatic_artwork(&mut state);
        assert!(state.automatic_artwork_retries.is_empty());
        assert_eq!(state.automatic_artwork_cursor, 2);
    }

    #[test]
    fn automatic_artwork_retries_queue_only_after_the_scan_cursor_passes() {
        let mut state = AppState {
            tracks: vec![
                Track {
                    source: TrackSource::Local,
                    id: 1,
                    path: PathBuf::from("first.mp3"),
                    title: "First".into(),
                    format: "MP3".into(),
                },
                Track {
                    source: TrackSource::Local,
                    id: 2,
                    path: PathBuf::from("second.mp3"),
                    title: "Second".into(),
                    format: "MP3".into(),
                },
            ],
            ..AppState::default()
        };

        assert!(queue_automatic_artwork_retry(&mut state, 2));
        assert!(state.automatic_artwork_retries.is_empty());

        state.automatic_artwork_cursor = 2;
        assert!(queue_automatic_artwork_retry(&mut state, 2));
        assert_eq!(state.automatic_artwork_retries, [2]);
        assert!(queue_automatic_artwork_retry(&mut state, 1));
        assert_eq!(state.automatic_artwork_retries, [2, 1]);
    }

    #[test]
    fn opened_album_defaults_to_disc_track_order_and_honors_explicit_sort() {
        let tracks = vec![
            Track {
                source: TrackSource::Local,
                id: 1,
                path: PathBuf::from("one.mp3"),
                title: "C track".into(),
                format: "MP3".into(),
            },
            Track {
                source: TrackSource::Local,
                id: 2,
                path: PathBuf::from("two.mp3"),
                title: "A track".into(),
                format: "MP3".into(),
            },
            Track {
                source: TrackSource::Local,
                id: 3,
                path: PathBuf::from("three.mp3"),
                title: "B track".into(),
                format: "MP3".into(),
            },
        ];
        let mut state = AppState {
            tracks,
            music_section: "albums".into(),
            open_music_group: Some(1),
            music_sort: "recent".into(),
            ..AppState::default()
        };
        state.details = [(1, 1), (2, 2), (3, 3)]
            .into_iter()
            .map(|(id, number)| {
                (
                    id,
                    MediaDetails {
                        album: "Album".into(),
                        artist: "Artist".into(),
                        disc_number: Some(1),
                        track_number: Some(number),
                        ..MediaDetails::default()
                    },
                )
            })
            .collect();

        assert_eq!(
            music_collection(&state)
                .iter()
                .map(|track| track.id)
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );

        state.music_sort = "title".into();
        state.music_sort_explicit = true;
        assert_eq!(
            music_collection(&state)
                .iter()
                .map(|track| track.id)
                .collect::<Vec<_>>(),
            [2, 3, 1]
        );
    }

    #[test]
    fn artist_sort_uses_the_displayed_artist_and_open_groups_keep_requested_order() {
        let tracks = vec![
            Track {
                source: TrackSource::Local,
                id: 1,
                path: PathBuf::from("zulu.mp3"),
                title: "Zulu song".into(),
                format: "MP3".into(),
            },
            Track {
                source: TrackSource::Local,
                id: 2,
                path: PathBuf::from("alpha.mp3"),
                title: "Alpha song".into(),
                format: "MP3".into(),
            },
            Track {
                source: TrackSource::Local,
                id: 3,
                path: PathBuf::from("beta.mp3"),
                title: "Beta song".into(),
                format: "MP3".into(),
            },
        ];
        let mut state = AppState {
            tracks: tracks.clone(),
            music_section: "artists".into(),
            music_sort: "artist".into(),
            open_music_group: Some(1),
            ..AppState::default()
        };
        state.details = HashMap::from([
            (
                1,
                MediaDetails {
                    artist: "Shared".into(),
                    album_artist: "Alpha".into(),
                    ..MediaDetails::default()
                },
            ),
            (
                2,
                MediaDetails {
                    artist: "Shared".into(),
                    album_artist: "Zulu".into(),
                    ..MediaDetails::default()
                },
            ),
            (
                3,
                MediaDetails {
                    artist: "Shared".into(),
                    album_artist: "Beta".into(),
                    ..MediaDetails::default()
                },
            ),
        ]);
        let mut sorted = tracks;
        sort_music_tracks(&mut sorted, &state);
        assert_eq!(
            sorted.iter().map(|track| track.id).collect::<Vec<_>>(),
            [1, 3, 2]
        );

        state.music_sort = "title".into();
        let group_tracks = music_collection(&state);
        assert_eq!(
            group_tracks
                .iter()
                .map(|track| track.id)
                .collect::<Vec<_>>(),
            [2, 3, 1]
        );

        state.music_section = "playlists".into();
        state.current_view = "music".into();
        state.open_music_group = None;
        state.opened_playlist = Some(7);
        state.playlists = PlaylistCollection::restore(
            vec![Playlist {
                id: 7,
                name: "Order".into(),
                track_ids: vec![3, 1, 2],
            }],
            Some(7),
        );
        assert_eq!(
            music_collection(&state)
                .iter()
                .map(|track| track.id)
                .collect::<Vec<_>>(),
            [3, 1, 2]
        );
    }

    #[test]
    fn persistence_snapshot_retains_library_preferences_progress_and_duplicate_playlist_items() {
        let mut state = AppState {
            current_view: "music".into(),
            volume: 73,
            rate: 1.25,
            ..AppState::default()
        };
        state.progress.insert(
            1,
            Progress {
                media_id: 1,
                position_ms: 42_000,
                duration_ms: 120_000,
                completed: false,
            },
        );
        state.details.insert(
            1,
            MediaDetails {
                album: "Album".into(),
                ..MediaDetails::default()
            },
        );
        state.playlists = PlaylistCollection::restore(
            vec![Playlist {
                id: 4,
                name: "Repeated".into(),
                track_ids: vec![1, 1],
            }],
            Some(4),
        );
        state.playlist_target = Some(4);
        let snapshot = library_snapshot(&state);
        assert_eq!(snapshot.current_view, "music");
        assert_eq!((snapshot.volume, snapshot.rate), (73, 1.25));
        assert_eq!(snapshot.selected_playlist, Some(4));
        assert_eq!(snapshot.playlists[0].track_ids, [1, 1]);
        assert_eq!(
            snapshot.progress,
            [Progress {
                media_id: 1,
                position_ms: 42_000,
                duration_ms: 120_000,
                completed: false
            }]
        );
        assert_eq!(snapshot.details[0].1.album, "Album");
    }
}
