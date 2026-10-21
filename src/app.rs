use crate::browse::{self, MusicGroup};
use crate::library::TrackSelection;
use crate::library_worker::{LibraryEvent, LibraryJob, LibraryWorker};
use crate::metadata::{ArtworkKind, ArtworkRequest, LocalMetadata};
use crate::model::{readable_filename, MediaDetails, Track, TrackId};
use crate::playback::frame::FrameMailbox;
use crate::playback::queue::RepeatMode;
use crate::playback::{
    send_command, PlaybackCommand, PlaybackSnapshot, PlaybackUiState, PlaybackWorker,
    MAX_MEDIA_DELAY_US, MAX_PLAYBACK_RATE, MAX_VOLUME_PERCENT, MIN_PLAYBACK_RATE,
    MIN_VOLUME_PERCENT, MOVIE_FORMAT,
};
use crate::playlists::{PlaylistCollection, PlaylistId};
use crate::scan::ScanOutcome;
use crate::series::episode_info;
use crate::storage::{self, LibraryData, Progress, StorageWorker};
use crate::{AppWindow, BrowseRow, QueueRow, TrackChoice, TrackRow};
use rfd::FileDialog;
use slint::{
    ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, Timer,
    TimerMode, VecModel,
};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{mpsc::SyncSender, Arc, Mutex};
use std::time::{Duration, Instant};

const TRANSPORT_DEBOUNCE: Duration = Duration::from_millis(220);
const TRANSPORT_ACK_TIMEOUT: Duration = Duration::from_secs(5);
const MUSIC_PAGE_SIZE: usize = 50;
const CARD_PAGE_SIZE: usize = 24;
const METADATA_BATCH_SIZE: usize = 32;
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
    jump_time_input: String,
    video_fill: bool,
    page_index: usize,
    visible_tracks: Vec<Track>,
    visible_playlist_positions: Vec<usize>,
    next_id: TrackId,
    feedback: String,
    library_status: String,
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
    playlist_targets_model: Rc<VecModel<SharedString>>,
    movie_genres_model: Rc<VecModel<SharedString>>,
    music_genres_model: Rc<VecModel<SharedString>>,
    rendered_queue_revision: u64,
    last_current_id: Option<TrackId>,
    last_current_load_generation: u64,
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
            jump_time_input: String::new(),
            video_fill: false,
            page_index: 0,
            visible_tracks: Vec::new(),
            visible_playlist_positions: Vec::new(),
            next_id: 1,
            feedback: String::new(),
            library_status: String::new(),
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
            playlist_targets_model: Rc::new(VecModel::default()),
            movie_genres_model: Rc::new(VecModel::default()),
            music_genres_model: Rc::new(VecModel::default()),
            rendered_queue_revision: u64::MAX,
            last_current_id: None,
            last_current_load_generation: 0,
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
    let (app_state, startup_metadata) = load_app_state();
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

    refresh_from_state(&ui.as_weak(), &state, &snapshot);
    wire_library_ui(&ui, &state, &command_sender, &snapshot);
    register_transport(&ui, &state, &command_sender, &snapshot);

    let weak = ui.as_weak();
    let state_timer = Rc::clone(&state);
    let sender_timer = command_sender.clone();
    let snapshot_timer = Arc::clone(&snapshot);
    let last_save = Rc::new(Cell::new(Instant::now()));
    let last_save_timer = Rc::clone(&last_save);
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_millis(180), move || {
        if let Some(ui) = weak.upgrade() {
            let playback = read_snapshot(&snapshot_timer);
            let mut state = state_timer.borrow_mut();
            flush_pending_transport(&ui, &mut state, &sender_timer, &snapshot_timer);
            process_library_events(&mut state);
            flush_pending_metadata(&mut state);
            schedule_automatic_artwork(&mut state);
            update_progress(&mut state, &playback);
            if last_save_timer.get().elapsed() >= PROGRESS_SAVE_INTERVAL {
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
    });

    let video_weak = ui.as_weak();
    let frames_timer = Arc::clone(&video_frames);
    let video_snapshot = Arc::clone(&snapshot);
    let displayed_generation = Rc::new(Cell::new(video_frames.generation()));
    let frame_timer = Timer::default();
    frame_timer.start(TimerMode::Repeated, Duration::from_millis(16), move || {
        if let Some(ui) = video_weak.upgrade() {
            let playback = read_snapshot(&video_snapshot);
            refresh_video_frame(
                &ui,
                &frames_timer,
                &displayed_generation,
                playback.current_is_video,
            );
        }
    });

    let result = ui.run();
    timer.stop();
    frame_timer.stop();
    let storage_error = {
        let mut state = state.borrow_mut();
        let playback = read_snapshot(&snapshot);
        process_library_events(&mut state);
        update_progress(&mut state, &playback);
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
        .map(|track| track.id)
        .collect();
    if let Ok(worker) = LibraryWorker::spawn(state.cache_dir.clone()) {
        state.library_worker = Some(worker);
    } else {
        state
            .library_status
            .push_str(" Artwork and folder scanning are unavailable.");
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
                play_collection(&mut state, &sender, &snapshot, queue, start, None);
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
                play_collection(&mut state, &sender, &snapshot, queue, 0, None);
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
                play_collection(&mut state, &sender, &snapshot, queue, 0, None);
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
                let result = state.library_worker.as_ref().ok_or_else(|| "Folder scanning is unavailable because the metadata worker could not start.".to_owned()).and_then(|worker| worker.submit(LibraryJob::Scan(root)));
                state.feedback = result.err().unwrap_or_else(|| "Scanning the selected folder in the background…".into());
                refresh_from_parts(&weak, &mut state, &snapshot);
            }
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
                let queued =
                    play_collection(&mut state, &sender, &snapshot, queue, index, resume_at);
                state.current_view = "movies".into();
                if queued {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_movie_fullscreen(true);
                        ui.window().set_fullscreen(true);
                    }
                }
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
    find_track(state, id).map(|track| track.path.clone())
}

fn add_paths(
    state: &mut AppState,
    paths: Vec<PathBuf>,
    force_video: Option<bool>,
) -> (usize, bool) {
    let mut added = 0;
    let mut metadata_ids = Vec::new();
    let mut metadata_available = true;
    for selected in paths {
        let path = std::fs::canonicalize(&selected).unwrap_or(selected);
        if state
            .tracks
            .iter()
            .chain(&state.movies)
            .any(|track| track.path == path)
        {
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
    }
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

fn process_library_events(state: &mut AppState) {
    let events = state
        .library_worker
        .as_ref()
        .map(LibraryWorker::drain_events)
        .unwrap_or_default();
    for event in events {
        match event {
            LibraryEvent::Scanned(outcome) => apply_scan_result(state, outcome),
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

fn apply_scan_result(state: &mut AppState, outcome: ScanOutcome) {
    let (count, metadata_available) = add_paths(state, outcome.paths, None);
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
    } else if metadata_available {
        state.feedback = "The scan found no new supported media files.".into();
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
    if !opened_playlist {
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
    let playlist = state
        .playlists
        .playlists()
        .iter()
        .find(|playlist| playlist.id == id);
    let mut tracks = browse::collection(
        &state.tracks,
        &state.details,
        "playlists",
        None,
        playlist,
        query,
    );
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
    let tracks: HashMap<_, _> = state.tracks.iter().map(|track| (track.id, track)).collect();
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
        detail,
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
            TrackRow {
                id: track.id,
                index: (offset + local_index) as i32,
                playlist_position,
                title: SharedString::from(track.title.as_str()),
                detail: SharedString::from(detail),
                genre: SharedString::from(nonempty(&details.genre, "—").unwrap_or("—")),
                duration: SharedString::from(duration),
                selected,
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
            TrackRow {
                id: movie.id,
                index: (offset + local_index) as i32,
                playlist_position: -1,
                title: SharedString::from(movie.title.as_str()),
                detail: SharedString::from(detail),
                genre: SharedString::from(nonempty(&details.genre, "—").unwrap_or("—")),
                duration: SharedString::from("—"),
                selected: false,
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
    ui.set_music_sort(SharedString::from(match state.music_sort.as_str() {
        "title" => "Title A–Z",
        "artist" => "Artist A–Z",
        _ => "Recently added",
    }));
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
            .map(|track| track.path.display().to_string())
            .unwrap_or_default(),
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
    let current_changed = state.last_current_id != playback.current_id
        || state.last_current_load_generation != playback.load_generation;
    if current_changed {
        state.last_current_id = playback.current_id;
        state.last_current_load_generation = playback.load_generation;
        state.audio_delay_input = format!("{}", playback.audio_delay_us / 1000);
        state.subtitle_delay_input = format!("{}", playback.subtitle_delay_us / 1000);
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
    playing_video: bool,
) {
    let generation = frames.generation();
    if displayed_generation.get() != generation {
        displayed_generation.set(generation);
        ui.set_video_frame(Image::default());
        ui.set_has_video_frame(false);
    }

    if !playing_video {
        if ui.get_has_video_frame() {
            ui.set_video_frame(Image::default());
            ui.set_has_video_frame(false);
        }
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
        }
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

fn movie_from_path(id: TrackId, path: std::path::PathBuf) -> Option<Track> {
    if !path.is_file() || !is_movie_path(&path) {
        return None;
    }

    Some(Track {
        id,
        title: readable_filename(&path),
        path,
        format: MOVIE_FORMAT.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        add_paths, apply_artwork_result, apply_automatic_artwork_result, apply_metadata_result,
        can_resume, can_toggle_pause, collection_start_index, collection_track_at,
        control_resolution, flush_pending_metadata, library_snapshot, movie_double_click,
        movie_from_path, music_collection, next_artwork_intent, pending_seek_matches_playback,
        play_collection, playlist_collection, playlist_filtered_positions, playlist_move_target,
        playlist_owner_for_queue, playlist_row_is_current, queue_automatic_artwork_retry,
        queue_position_label, refresh_track_choice_models, schedule_automatic_artwork,
        seek_commit_target, selected_displayed_tracks, set_movie_artwork_target, sort_music_tracks,
        status_message, AppState, ControlResolution, PendingSeek, MOVIE_DETAILS_CLICK_DELAY,
        TRANSPORT_ACK_TIMEOUT,
    };
    use crate::metadata::{ArtworkKind, ArtworkRequest, LocalMetadata};
    use crate::model::{MediaDetails, Track};
    use crate::playback::{
        is_movie, ControlAcknowledgement, PlaybackCommand, PlaybackSnapshot, PlaybackTrack,
        PlaybackUiState,
    };
    use crate::playlists::{Playlist, PlaylistCollection};
    use crate::storage::Progress;
    use slint::{Model, ModelRc, SharedString, VecModel};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::{Duration, Instant};
    use std::time::{SystemTime, UNIX_EPOCH};

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
    fn movie_resume_position_travels_with_the_queue_command() {
        let movie = Track {
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
            id: 1,
            path: PathBuf::from("first.mp3"),
            title: "First".into(),
            format: "MP3".into(),
        };
        let second = Track {
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
                id: 1,
                path: PathBuf::from("one.mp3"),
                title: "One".into(),
                format: "MP3".into(),
            },
            Track {
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
                id: 1,
                path: PathBuf::from("one.mp3"),
                title: "One".into(),
                format: "MP3".into(),
            },
            Track {
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
            id: 1,
            path: PathBuf::from("one.mp3"),
            title: "One".into(),
            format: "MP3".into(),
        };
        let two = Track {
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
                id: 1,
                path: PathBuf::from("one.mp3"),
                title: "One".into(),
                format: "MP3".into(),
            },
            Track {
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
                    id: 1,
                    path: PathBuf::from("first.mp3"),
                    title: "First".into(),
                    format: "MP3".into(),
                },
                Track {
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
                    id: 1,
                    path: PathBuf::from("first.mp3"),
                    title: "First".into(),
                    format: "MP3".into(),
                },
                Track {
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
    fn artist_sort_uses_the_displayed_artist_and_open_groups_keep_requested_order() {
        let tracks = vec![
            Track {
                id: 1,
                path: PathBuf::from("zulu.mp3"),
                title: "Zulu song".into(),
                format: "MP3".into(),
            },
            Track {
                id: 2,
                path: PathBuf::from("alpha.mp3"),
                title: "Alpha song".into(),
                format: "MP3".into(),
            },
            Track {
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
