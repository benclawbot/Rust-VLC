use crate::library::{visible_tracks, TrackSelection};
use crate::model::{Track, TrackId};
use crate::playback::{
    send_command, PlaybackCommand, PlaybackSnapshot, PlaybackUiState, PlaybackWorker,
};
use crate::playlists::PlaylistCollection;
use crate::{AppWindow, PlaylistRow, TrackRow};
use rfd::FileDialog;
use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{mpsc::SyncSender, Arc, Mutex};
use std::time::Duration;

struct AppState {
    tracks: Vec<Track>,
    selection: TrackSelection,
    playlists: PlaylistCollection,
    query: String,
    new_playlist_name: String,
    current_view: String,
    next_id: TrackId,
    feedback: String,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            tracks: Vec::new(),
            selection: TrackSelection::default(),
            playlists: PlaylistCollection::default(),
            query: String::new(),
            new_playlist_name: String::new(),
            current_view: "music".to_owned(),
            next_id: 1,
            feedback: String::new(),
        }
    }
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let ui = AppWindow::new()?;
    let mut worker = PlaybackWorker::spawn()?;
    let state = Rc::new(RefCell::new(AppState::default()));
    let command_sender = worker.command_sender();
    let snapshot = worker.shared_snapshot();

    refresh_ui(&ui, &state.borrow(), &read_snapshot(&snapshot));

    {
        let state = Rc::clone(&state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(&snapshot);
        ui.on_navigate(move |view| {
            let mut state = state.borrow_mut();
            state.current_view = if view.as_str() == "movies" {
                "movies".to_owned()
            } else {
                "music".to_owned()
            };
            // Browsing Movies/Music is a view change only. It never sends a
            // command to the playback owner, so the current audio queue keeps running.
            if let Some(ui) = weak.upgrade() {
                refresh_ui(&ui, &state, &read_snapshot(&snapshot));
            }
        });
    }

    {
        let state = Rc::clone(&state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(&snapshot);
        ui.on_shuffle_selected_playlist(move || {
            let mut state = state.borrow_mut();
            let playlist_name = state
                .playlists
                .selected_playlist()
                .map(|playlist| playlist.name.clone())
                .unwrap_or_else(|| "playlist".to_owned());
            match state.playlists.shuffle_selected() {
                Ok(()) => {
                    state.feedback = format!(
                        "Shuffled the order of {playlist_name}. Playlist changes last for this session."
                    );
                }
                Err(error) => state.feedback = error.to_string(),
            }
            if let Some(ui) = weak.upgrade() {
                refresh_ui(&ui, &state, &read_snapshot(&snapshot));
            }
        });
    }

    {
        let state = Rc::clone(&state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(&snapshot);
        ui.on_open_audio_files(move || {
            let picked = FileDialog::new()
                .add_filter(
                    "Audio files",
                    &[
                        "aac", "aif", "aiff", "alac", "ape", "flac", "m4a", "m4b", "mid",
                        "midi", "mp3", "mpc", "oga", "ogg", "opus", "wav", "wma", "wv",
                    ],
                )
                .pick_files();

            let Some(paths) = picked else {
                return;
            };

            let mut state = state.borrow_mut();
            let mut added = 0usize;
            let mut rejected = 0usize;
            for selected_path in paths {
                let path = std::fs::canonicalize(&selected_path).unwrap_or(selected_path);
                if state.tracks.iter().any(|track| track.path == path) {
                    continue;
                }
                match Track::from_path(state.next_id, path) {
                    Some(track) => {
                        state.next_id = state.next_id.saturating_add(1);
                        state.tracks.push(track);
                        added += 1;
                    }
                    None => rejected += 1,
                }
            }

            state.feedback = if added == 0 && rejected > 0 {
                "No supported audio files were added. Choose a readable local audio file.".to_owned()
            } else if added == 0 {
                "Those audio files are already in this session's library.".to_owned()
            } else if rejected > 0 {
                format!("Added {added} audio files; skipped {rejected} unsupported or unavailable files.")
            } else {
                format!("Added {added} audio files to this session's library.")
            };
            if added > 0 {
                state.current_view = "music".to_owned();
            }

            if let Some(ui) = weak.upgrade() {
                refresh_ui(&ui, &state, &read_snapshot(&snapshot));
            }
        });
    }

    {
        let state = Rc::clone(&state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(&snapshot);
        ui.on_search_changed(move |query| {
            let mut state = state.borrow_mut();
            state.query = query.to_string();
            state.feedback.clear();
            if let Some(ui) = weak.upgrade() {
                refresh_ui(&ui, &state, &read_snapshot(&snapshot));
            }
        });
    }

    {
        let state = Rc::clone(&state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(&snapshot);
        ui.on_select_all(move |selected| {
            let mut state = state.borrow_mut();
            let query = state.query.clone();
            let tracks = state.tracks.clone();
            state.selection.set_visible(&tracks, &query, selected);
            state.feedback.clear();
            if let Some(ui) = weak.upgrade() {
                refresh_ui(&ui, &state, &read_snapshot(&snapshot));
            }
        });
    }

    {
        let state = Rc::clone(&state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(&snapshot);
        ui.on_playlist_name_changed(move |name| {
            let mut state = state.borrow_mut();
            state.new_playlist_name = name.to_string();
            if let Some(ui) = weak.upgrade() {
                refresh_ui(&ui, &state, &read_snapshot(&snapshot));
            }
        });
    }

    {
        let state = Rc::clone(&state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(&snapshot);
        ui.on_create_playlist(move |name| {
            let mut state = state.borrow_mut();
            match state.playlists.create(name.as_str()) {
                Ok(_) => {
                    state.new_playlist_name.clear();
                    state.feedback = "Playlist created for this session.".to_owned();
                }
                Err(error) => state.feedback = error.to_string(),
            }
            if let Some(ui) = weak.upgrade() {
                refresh_ui(&ui, &state, &read_snapshot(&snapshot));
            }
        });
    }

    {
        let state = Rc::clone(&state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(&snapshot);
        ui.on_select_playlist(move |id| {
            let mut state = state.borrow_mut();
            if !state.playlists.select(id) {
                state.feedback = "That playlist is no longer available.".to_owned();
            } else {
                state.feedback.clear();
            }
            if let Some(ui) = weak.upgrade() {
                refresh_ui(&ui, &state, &read_snapshot(&snapshot));
            }
        });
    }

    {
        let state = Rc::clone(&state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(&snapshot);
        ui.on_add_selected_to_playlist(move || {
            let mut state = state.borrow_mut();
            let selected_tracks = state
                .selection
                .selected_visible_in_order(&state.tracks, &state.query);
            match state.playlists.add_tracks(&selected_tracks) {
                Ok(0) => {
                    state.feedback = "All checked tracks are already in this playlist.".to_owned();
                }
                Ok(added) => {
                    let playlist_name = state
                        .playlists
                        .playlists()
                        .iter()
                        .find(|playlist| Some(playlist.id) == state.playlists.selected_id())
                        .map(|playlist| playlist.name.as_str())
                        .unwrap_or("playlist");
                    state.feedback = format!(
                        "Added {added} checked tracks to {playlist_name}. This playlist is session-only."
                    );
                }
                Err(error) => state.feedback = error.to_string(),
            }
            if let Some(ui) = weak.upgrade() {
                refresh_ui(&ui, &state, &read_snapshot(&snapshot));
            }
        });
    }

    {
        let state = Rc::clone(&state);
        let weak = ui.as_weak();
        let snapshot = Arc::clone(&snapshot);
        ui.on_toggle_track(move |id, selected| {
            let mut state = state.borrow_mut();
            state.selection.set(id, selected);
            state.feedback.clear();
            if let Some(ui) = weak.upgrade() {
                refresh_ui(&ui, &state, &read_snapshot(&snapshot));
            }
        });
    }

    {
        let state = Rc::clone(&state);
        let sender = command_sender.clone();
        let weak = ui.as_weak();
        let snapshot = Arc::clone(&snapshot);
        ui.on_play_track(move |id| {
            let mut state = state.borrow_mut();
            let visible = visible_tracks(&state.tracks, &state.query);
            let start_index = visible.iter().position(|track| track.id == id);
            if let Some(start_index) = start_index {
                let queue = visible.into_iter().cloned().collect();
                state.feedback.clear();
                let _ = send_command(
                    &sender,
                    PlaybackCommand::PlayQueue {
                        tracks: queue,
                        start_index,
                    },
                    &snapshot,
                );
            }
            if let Some(ui) = weak.upgrade() {
                refresh_ui(&ui, &state, &read_snapshot(&snapshot));
            }
        });
    }

    {
        let state = Rc::clone(&state);
        let sender = command_sender.clone();
        let weak = ui.as_weak();
        let snapshot = Arc::clone(&snapshot);
        ui.on_play_selected(move || {
            let mut state = state.borrow_mut();
            let queue = state
                .selection
                .selected_visible_in_order(&state.tracks, &state.query);
            if queue.is_empty() {
                state.feedback = "Select one or more tracks first.".to_owned();
            } else {
                state.feedback.clear();
                let _ = send_command(
                    &sender,
                    PlaybackCommand::PlayQueue {
                        tracks: queue,
                        start_index: 0,
                    },
                    &snapshot,
                );
            }
            if let Some(ui) = weak.upgrade() {
                refresh_ui(&ui, &state, &read_snapshot(&snapshot));
            }
        });
    }

    register_transport(&ui, &state, &command_sender, &snapshot);

    let weak = ui.as_weak();
    let state = Rc::clone(&state);
    let snapshot_for_timer = Arc::clone(&snapshot);
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_millis(180), move || {
        let snapshot = read_snapshot(&snapshot_for_timer);
        if let Some(ui) = weak.upgrade() {
            refresh_playback_ui(&ui, &state.borrow(), &snapshot);
        }
    });

    let result = ui.run();
    timer.stop();
    worker.shutdown();
    result?;
    Ok(())
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
            let _ = send_command(&sender, PlaybackCommand::Previous, &snapshot);
            if let Some(ui) = weak.upgrade() {
                refresh_ui(&ui, &app_state.borrow(), &read_snapshot(&snapshot));
            }
        });
    }

    {
        let app_state = Rc::clone(app_state);
        let sender = sender.clone();
        let snapshot = Arc::clone(snapshot);
        let weak = ui.as_weak();
        ui.on_toggle_pause(move || {
            let _ = send_command(&sender, PlaybackCommand::TogglePause, &snapshot);
            if let Some(ui) = weak.upgrade() {
                refresh_ui(&ui, &app_state.borrow(), &read_snapshot(&snapshot));
            }
        });
    }

    {
        let app_state = Rc::clone(app_state);
        let sender = sender.clone();
        let snapshot = Arc::clone(snapshot);
        let weak = ui.as_weak();
        ui.on_next(move || {
            let _ = send_command(&sender, PlaybackCommand::Next, &snapshot);
            if let Some(ui) = weak.upgrade() {
                refresh_ui(&ui, &app_state.borrow(), &read_snapshot(&snapshot));
            }
        });
    }
}

fn refresh_ui(ui: &AppWindow, state: &AppState, playback: &PlaybackSnapshot) {
    let visible = visible_tracks(&state.tracks, &state.query);
    let selected_count = visible
        .iter()
        .filter(|track| state.selection.contains(track.id))
        .count();
    let rows: Vec<TrackRow> = visible
        .iter()
        .map(|track| TrackRow {
            id: track.id,
            title: SharedString::from(track.title.as_str()),
            detail: SharedString::from(format!("{} · local file", track.format)),
            selected: state.selection.contains(track.id),
        })
        .collect();
    let playlist_rows: Vec<PlaylistRow> = state
        .playlists
        .playlists()
        .iter()
        .map(|playlist| PlaylistRow {
            id: playlist.id,
            name: SharedString::from(playlist.name.as_str()),
            track_count: playlist.track_ids.len() as i32,
        })
        .collect();
    let selected_playlist_order = state
        .playlists
        .selected_playlist()
        .map(|playlist| {
            playlist
                .track_ids
                .iter()
                .filter_map(|id| state.tracks.iter().find(|track| track.id == *id))
                .map(|track| track.title.as_str())
                .collect::<Vec<_>>()
                .join(" · ")
        })
        .unwrap_or_else(|| "Select a playlist to view its track order.".to_owned());

    ui.set_current_view(SharedString::from(state.current_view.as_str()));
    ui.set_tracks(ModelRc::new(VecModel::from(rows)));
    ui.set_playlists(ModelRc::new(VecModel::from(playlist_rows)));
    ui.set_has_tracks(!state.tracks.is_empty());
    ui.set_has_selection(selected_count > 0);
    ui.set_has_selected_playlist(state.playlists.has_selected_playlist());
    ui.set_can_shuffle_selected_playlist(state.playlists.can_shuffle_selected());
    ui.set_selected_playlist_id(state.playlists.selected_id().unwrap_or(-1));
    ui.set_selected_playlist_order(SharedString::from(selected_playlist_order));
    ui.set_select_all_checked(
        state
            .selection
            .all_visible_selected(&state.tracks, &state.query),
    );
    ui.set_select_all_enabled(!visible.is_empty());
    ui.set_selected_count(selected_count as i32);
    ui.set_visible_count(visible.len() as i32);
    ui.set_search_query(SharedString::from(state.query.as_str()));
    ui.set_new_playlist_name(SharedString::from(state.new_playlist_name.as_str()));
    refresh_playback_ui(ui, state, playback);
}

fn refresh_playback_ui(ui: &AppWindow, state: &AppState, playback: &PlaybackSnapshot) {
    ui.set_runtime_ready(playback.runtime_checked);
    ui.set_runtime_available(playback.runtime_available);
    ui.set_runtime_message(SharedString::from(playback.runtime_message.as_str()));
    ui.set_has_current_track(playback.current_id.is_some());
    ui.set_current_title(SharedString::from(if playback.current_title.is_empty() {
        "No track selected"
    } else {
        playback.current_title.as_str()
    }));
    ui.set_player_state(SharedString::from(playback.state.label()));
    ui.set_player_time(SharedString::from(format_time(playback.position_ms)));
    ui.set_player_duration(SharedString::from(format_time(playback.duration_ms)));
    ui.set_queue_position(SharedString::from(if playback.queue_len == 0 {
        "No queue".to_owned()
    } else {
        format!(
            "{} of {} in queue",
            playback.queue_index, playback.queue_len
        )
    }));
    ui.set_can_toggle_pause(
        playback.runtime_available
            && playback.current_id.is_some()
            && matches!(
                playback.state,
                PlaybackUiState::Loading | PlaybackUiState::Playing | PlaybackUiState::Paused
            ),
    );
    ui.set_is_paused(playback.state == PlaybackUiState::Paused);
    ui.set_status_message(SharedString::from(if state.feedback.is_empty() {
        playback.message.as_str()
    } else {
        state.feedback.as_str()
    }));
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
