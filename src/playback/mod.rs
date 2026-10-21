mod ffi;
pub mod frame;
pub mod queue;

use crate::model::{Track, TrackId};
use crate::playback::ffi::{PlayerState, VlcEngine};
use crate::playback::frame::FrameMailbox;
use crate::playback::queue::{PlaybackQueue, RepeatMode};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const COMMAND_CAPACITY: usize = 32;
const POLL_INTERVAL: Duration = Duration::from_millis(160);
const RESUME_SEEK_READY_TIMEOUT: Duration = Duration::from_secs(10);
// App-generated seek IDs start at 1, so zero marks a queue-owned resume seek.
const RESUME_SEEK_REQUEST_ID: u64 = 0;
pub const MIN_VOLUME_PERCENT: i32 = 0;
pub const MAX_VOLUME_PERCENT: i32 = 100;
pub const MIN_PLAYBACK_RATE: f32 = 0.25;
pub const MAX_PLAYBACK_RATE: f32 = 4.0;
pub const MAX_MEDIA_DELAY_US: i64 = 60_000_000;
pub const MOVIE_FORMAT: &str = "MOVIE";

pub fn is_movie(track: &Track) -> bool {
    track.format == MOVIE_FORMAT
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaybackTrack {
    pub id: i32,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct PlaybackSnapshot {
    pub runtime_checked: bool,
    pub runtime_available: bool,
    pub runtime_message: String,
    pub current_id: Option<TrackId>,
    pub load_generation: u64,
    pub current_is_video: bool,
    pub current_title: String,
    pub state: PlaybackUiState,
    pub queue_index: usize,
    pub queue_len: usize,
    pub queue_titles: Vec<String>,
    pub queue_revision: u64,
    pub repeat_mode: RepeatMode,
    pub shuffle: bool,
    pub position_ms: i64,
    pub duration_ms: i64,
    pub audio_tracks: Vec<PlaybackTrack>,
    pub subtitle_tracks: Vec<PlaybackTrack>,
    pub chapters: Vec<PlaybackTrack>,
    pub audio_track: i32,
    pub subtitle_track: i32,
    pub chapter: i32,
    pub audio_delay_us: i64,
    pub subtitle_delay_us: i64,
    pub volume_percent: i32,
    pub playback_rate: f32,
    pub seek_ack: ControlAcknowledgement,
    pub volume_ack: ControlAcknowledgement,
    pub playback_rate_ack: ControlAcknowledgement,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ControlAcknowledgement {
    pub request_id: u64,
    pub succeeded: bool,
}

impl Default for PlaybackSnapshot {
    fn default() -> Self {
        Self {
            runtime_checked: false,
            runtime_available: false,
            runtime_message: String::new(),
            current_id: None,
            load_generation: 0,
            current_is_video: false,
            current_title: String::new(),
            state: PlaybackUiState::Idle,
            queue_index: 0,
            queue_len: 0,
            queue_titles: Vec::new(),
            queue_revision: 0,
            repeat_mode: RepeatMode::Off,
            shuffle: false,
            position_ms: 0,
            duration_ms: 0,
            audio_tracks: Vec::new(),
            subtitle_tracks: Vec::new(),
            chapters: Vec::new(),
            audio_track: -1,
            subtitle_track: -1,
            chapter: -1,
            audio_delay_us: 0,
            subtitle_delay_us: 0,
            volume_percent: 100,
            playback_rate: 1.0,
            seek_ack: ControlAcknowledgement::default(),
            volume_ack: ControlAcknowledgement::default(),
            playback_rate_ack: ControlAcknowledgement::default(),
            message: String::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PlaybackUiState {
    #[default]
    Idle,
    Loading,
    Playing,
    Paused,
    Ended,
    Error,
    Unavailable,
}

impl PlaybackUiState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "Ready",
            Self::Loading => "Opening",
            Self::Playing => "Playing",
            Self::Paused => "Paused",
            Self::Ended => "Finished",
            Self::Error => "Playback error",
            Self::Unavailable => "VLC unavailable",
        }
    }
}

#[derive(Debug)]
pub enum PlaybackCommand {
    PlayQueue {
        tracks: Vec<Track>,
        start_index: usize,
        resume_position_ms: Option<i64>,
    },
    TogglePause,
    Stop,
    SeekTo {
        request_id: u64,
        track_id: TrackId,
        load_generation: u64,
        position_ms: i64,
    },
    SetVolume {
        request_id: u64,
        volume_percent: i32,
    },
    SetPlaybackRate {
        request_id: u64,
        rate: f32,
    },
    Previous,
    Next,
    SetRepeat(RepeatMode),
    SetShuffle(bool),
    SelectAudioTrack {
        media_id: i32,
        load_generation: u64,
        track_id: i32,
    },
    SelectSubtitleTrack {
        media_id: i32,
        load_generation: u64,
        track_id: i32,
    },
    AddSubtitle {
        media_id: i32,
        load_generation: u64,
        path: std::path::PathBuf,
    },
    SetAudioDelay {
        media_id: i32,
        load_generation: u64,
        delay_us: i64,
    },
    SetSubtitleDelay {
        media_id: i32,
        load_generation: u64,
        delay_us: i64,
    },
    SelectChapter {
        media_id: i32,
        load_generation: u64,
        chapter: i32,
    },
    QueueJump {
        revision: u64,
        position: usize,
    },
    QueueMove {
        revision: u64,
        from: usize,
        to: usize,
    },
    QueueRemove {
        revision: u64,
        position: usize,
    },
    RefreshOwningQueue {
        revision: u64,
        tracks: Vec<Track>,
    },
    PlayNext {
        track: Track,
    },
    Shutdown,
}

pub struct PlaybackWorker {
    sender: SyncSender<PlaybackCommand>,
    snapshot: Arc<Mutex<PlaybackSnapshot>>,
    frames: Arc<FrameMailbox>,
    join: Option<JoinHandle<()>>,
}

impl PlaybackWorker {
    pub fn spawn() -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        let snapshot = Arc::new(Mutex::new(PlaybackSnapshot::default()));
        let frames = Arc::new(FrameMailbox::default());
        let worker_snapshot = Arc::clone(&snapshot);
        let worker_frames = Arc::clone(&frames);
        let join = thread::Builder::new()
            .name("vlc-playback-owner".to_owned())
            .spawn(move || run_worker(receiver, worker_snapshot, worker_frames))?;

        Ok(Self {
            sender,
            snapshot,
            frames,
            join: Some(join),
        })
    }

    pub fn command_sender(&self) -> SyncSender<PlaybackCommand> {
        self.sender.clone()
    }

    pub fn snapshot(&self) -> PlaybackSnapshot {
        lock_snapshot(&self.snapshot).clone()
    }

    pub fn shared_snapshot(&self) -> Arc<Mutex<PlaybackSnapshot>> {
        Arc::clone(&self.snapshot)
    }

    pub fn video_frames(&self) -> Arc<FrameMailbox> {
        Arc::clone(&self.frames)
    }

    pub fn submit(&self, command: PlaybackCommand) -> Result<(), String> {
        send_command(&self.sender, command, &self.snapshot)
    }

    pub fn shutdown(&mut self) {
        if let Some(join) = self.join.take() {
            let _ = self.sender.send(PlaybackCommand::Shutdown);
            let _ = join.join();
        }
    }
}

impl Drop for PlaybackWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub fn send_command(
    sender: &SyncSender<PlaybackCommand>,
    command: PlaybackCommand,
    snapshot: &Arc<Mutex<PlaybackSnapshot>>,
) -> Result<(), String> {
    if let Some(message) = invalid_command_message(&command) {
        lock_snapshot(snapshot).message = message.clone();
        return Err(message);
    }
    match sender.try_send(command) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(_)) => {
            let message = "Playback is busy. Wait a moment, then try again.".to_owned();
            lock_snapshot(snapshot).message = message.clone();
            Err(message)
        }
        Err(TrySendError::Disconnected(_)) => {
            let message = "The playback worker has stopped. Restart the application to play audio."
                .to_owned();
            lock_snapshot(snapshot).message = message.clone();
            Err(message)
        }
    }
}

fn invalid_command_message(command: &PlaybackCommand) -> Option<String> {
    match command {
        PlaybackCommand::SeekTo { position_ms, .. } if *position_ms < 0 => {
            Some("Seek position cannot be negative.".to_owned())
        }
        PlaybackCommand::PlayQueue {
            resume_position_ms: Some(position_ms),
            ..
        } if *position_ms < 0 => Some("Resume position cannot be negative.".to_owned()),
        PlaybackCommand::SetVolume { volume_percent, .. }
            if !valid_volume_percent(*volume_percent) =>
        {
            Some("Volume must be between 0% and 100%.".to_owned())
        }
        PlaybackCommand::SetPlaybackRate { rate, .. } if !valid_playback_rate(*rate) => {
            Some("Playback rate must be between 0.25× and 4×.".to_owned())
        }
        PlaybackCommand::SelectAudioTrack { track_id, .. } if *track_id < 0 => {
            Some("Choose an available audio track.".to_owned())
        }
        PlaybackCommand::SelectSubtitleTrack { track_id, .. } if *track_id < -1 => {
            Some("Choose an available subtitle track or Off.".to_owned())
        }
        PlaybackCommand::AddSubtitle { path, .. } if !path.is_file() => Some(format!(
            "The subtitle file is no longer available: {}",
            path.display()
        )),
        PlaybackCommand::SetAudioDelay { delay_us, .. } if !valid_media_delay(*delay_us) => {
            Some("Audio delay must be between −60 and +60 seconds.".to_owned())
        }
        PlaybackCommand::SetSubtitleDelay { delay_us, .. } if !valid_media_delay(*delay_us) => {
            Some("Subtitle delay must be between −60 and +60 seconds.".to_owned())
        }
        PlaybackCommand::SelectChapter { chapter, .. } if *chapter < 0 => {
            Some("Choose a chapter from the current media.".to_owned())
        }
        _ => None,
    }
}

fn valid_media_delay(delay_us: i64) -> bool {
    (-MAX_MEDIA_DELAY_US..=MAX_MEDIA_DELAY_US).contains(&delay_us)
}

fn valid_volume_percent(volume: i32) -> bool {
    (MIN_VOLUME_PERCENT..=MAX_VOLUME_PERCENT).contains(&volume)
}

fn valid_playback_rate(rate: f32) -> bool {
    rate.is_finite() && (MIN_PLAYBACK_RATE..=MAX_PLAYBACK_RATE).contains(&rate)
}

fn seek_target_ms(position_ms: i64, duration_ms: i64) -> Option<i64> {
    (position_ms >= 0 && duration_ms > 0).then(|| position_ms.min(duration_ms))
}

fn lock_snapshot(snapshot: &Mutex<PlaybackSnapshot>) -> MutexGuard<'_, PlaybackSnapshot> {
    snapshot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn publish(snapshot: &Mutex<PlaybackSnapshot>, update: impl FnOnce(&mut PlaybackSnapshot)) {
    update(&mut lock_snapshot(snapshot));
}

fn acknowledgement(request_id: u64, succeeded: bool) -> ControlAcknowledgement {
    ControlAcknowledgement {
        request_id,
        succeeded,
    }
}

fn cancel_pending_seek(
    pending_seek: &mut Option<(u64, TrackId, u64, i64, Option<Instant>)>,
    snapshot: &Mutex<PlaybackSnapshot>,
    message: &str,
) {
    if let Some((request_id, _, _, _, _)) = pending_seek.take() {
        publish(snapshot, |state| {
            state.message = message.to_owned();
            state.seek_ack = acknowledgement(request_id, false);
        });
    }
}

fn bind_resume_seek(worker: &mut WorkerState, position_ms: Option<i64>) {
    let Some(position_ms) = position_ms else {
        return;
    };
    let Some(track_id) = worker.queue.current().map(|track| track.id) else {
        return;
    };
    worker.pending_seek = Some((
        RESUME_SEEK_REQUEST_ID,
        track_id,
        worker.load_generation,
        position_ms,
        Some(Instant::now() + RESUME_SEEK_READY_TIMEOUT),
    ));
}

fn pending_seek_waits_for_ready(
    request_id: u64,
    player_state: PlayerState,
    duration_ms: i64,
    resume_deadline: Option<Instant>,
    now: Instant,
) -> bool {
    if request_id == RESUME_SEEK_REQUEST_ID {
        resume_deadline.is_some_and(|deadline| now < deadline)
            && match player_state {
                PlayerState::Opening
                | PlayerState::Buffering
                | PlayerState::Stopped
                | PlayerState::Other => true,
                PlayerState::Playing | PlayerState::Paused => duration_ms <= 0,
                PlayerState::Ended | PlayerState::Error => false,
            }
    } else {
        matches!(player_state, PlayerState::Opening | PlayerState::Buffering)
    }
}

fn prepare_replay(worker: &mut WorkerState, snapshot: &Mutex<PlaybackSnapshot>) {
    worker.generation = worker.generation.wrapping_add(1);
    worker.load_generation = worker.load_generation.wrapping_add(1);
    worker.ended_generation = None;
    cancel_pending_seek(
        &mut worker.pending_seek,
        snapshot,
        "Seek was canceled because playback restarted.",
    );
    publish(snapshot, |state| {
        state.load_generation = worker.load_generation
    });
}

struct WorkerState {
    queue: PlaybackQueue,
    queue_revision: u64,
    load_generation: u64,
    ended_generation: Option<u64>,
    generation: u64,
    playback_rate: f32,
    pending_seek: Option<(u64, TrackId, u64, i64, Option<Instant>)>,
}

impl Default for WorkerState {
    fn default() -> Self {
        Self {
            queue: PlaybackQueue::default(),
            queue_revision: 0,
            load_generation: 0,
            ended_generation: None,
            generation: 0,
            playback_rate: 1.0,
            pending_seek: None,
        }
    }
}

fn run_worker(
    receiver: Receiver<PlaybackCommand>,
    snapshot: Arc<Mutex<PlaybackSnapshot>>,
    frames: Arc<FrameMailbox>,
) {
    let mut engine = match VlcEngine::new(Arc::clone(&frames)) {
        Ok(engine) => {
            let runtime_message = format!("libVLC {} ready", engine.version());
            publish(&snapshot, |state| {
                state.runtime_checked = true;
                state.runtime_available = true;
                state.runtime_message = runtime_message;
                state.message = "Choose a local media file to begin.".to_owned();
            });
            Some(engine)
        }
        Err(error) => {
            let message = error.to_string();
            publish(&snapshot, |state| {
                state.runtime_checked = true;
                state.runtime_available = false;
                state.runtime_message = message.clone();
                state.state = PlaybackUiState::Unavailable;
                state.message = message;
            });
            None
        }
    };

    let runtime_error = lock_snapshot(&snapshot).runtime_message.clone();
    let mut state = WorkerState::default();

    loop {
        match receiver.recv_timeout(POLL_INTERVAL) {
            Ok(PlaybackCommand::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(command) => handle_command(
                command,
                &mut engine,
                &mut state,
                &snapshot,
                &frames,
                &runtime_error,
            ),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }

        let Some(engine) = engine.as_mut() else {
            continue;
        };
        if let Some((request_id, track_id, load_generation, position_ms, resume_deadline)) =
            state.pending_seek
        {
            if state.queue.current().map(|track| track.id) != Some(track_id)
                || state.load_generation != load_generation
            {
                state.pending_seek = None;
                publish(&snapshot, |state| {
                    state.message =
                        "Seek was canceled because the current media changed.".to_owned();
                    state.seek_ack = acknowledgement(request_id, false);
                });
                continue;
            }
            let player_state = engine.state();
            let duration_ms = engine.length_ms();
            let now = Instant::now();
            if request_id == RESUME_SEEK_REQUEST_ID
                && matches!(player_state, PlayerState::Error | PlayerState::Ended)
            {
                state.pending_seek = None;
                publish(&snapshot, |state| {
                    state.seek_ack = acknowledgement(request_id, false);
                });
            } else if request_id == RESUME_SEEK_REQUEST_ID
                && resume_deadline.is_none_or(|deadline| now >= deadline)
            {
                state.pending_seek = None;
                publish(&snapshot, |state| {
                    state.message = "The track did not become seekable.".to_owned();
                    state.seek_ack = acknowledgement(request_id, false);
                });
            } else if pending_seek_waits_for_ready(
                request_id,
                player_state,
                duration_ms,
                resume_deadline,
                now,
            ) {
                // Keep resume pending while VLC moves through startup states.
            } else {
                match (player_state, duration_ms > 0, engine.is_seekable()) {
                    (PlayerState::Opening | PlayerState::Buffering, _, _) => {}
                    (PlayerState::Playing | PlayerState::Paused, true, true) => {
                        state.pending_seek = None;
                        apply_seek(engine, request_id, position_ms, &state.queue, &snapshot);
                    }
                    (PlayerState::Playing | PlayerState::Paused, true, false) => {
                        state.pending_seek = None;
                        publish(&snapshot, |state| {
                            state.message =
                                "VLC reports that the current media is not seekable.".to_owned();
                            state.seek_ack = acknowledgement(request_id, false);
                        });
                    }
                    _ => {
                        state.pending_seek = None;
                        publish(&snapshot, |state| {
                            state.message = "The track did not become seekable.".to_owned();
                            state.seek_ack = acknowledgement(request_id, false);
                        });
                    }
                }
            }
        }
        if state.queue.current().is_none() {
            continue;
        }
        if lock_snapshot(&snapshot).state == PlaybackUiState::Error {
            refresh_media_snapshot(engine, &state.queue, &snapshot);
            continue;
        }

        let player_state = engine.state();
        if state.ended_generation == Some(state.generation)
            && matches!(
                player_state,
                PlayerState::Opening
                    | PlayerState::Buffering
                    | PlayerState::Playing
                    | PlayerState::Paused
            )
        {
            state.ended_generation = None;
        }
        match player_state {
            PlayerState::Ended if state.ended_generation != Some(state.generation) => {
                state.ended_generation = Some(state.generation);
                if state.queue.advance_on_end().is_some() {
                    state.generation = state.generation.wrapping_add(1);
                    state.load_generation = state.load_generation.wrapping_add(1);
                    state.ended_generation = None;
                    cancel_pending_seek(
                        &mut state.pending_seek,
                        &snapshot,
                        "Seek was canceled because the playback item changed.",
                    );
                    start_current(
                        engine,
                        &state.queue,
                        state.queue_revision,
                        state.load_generation,
                        &snapshot,
                        state.playback_rate,
                    );
                } else {
                    publish_player_position(engine, &state.queue, &snapshot, "Queue finished.");
                    publish(&snapshot, |state| state.state = PlaybackUiState::Ended);
                }
            }
            PlayerState::Error => {
                let error = engine.error_message();
                let message = if error.is_empty() {
                    "VLC could not decode this media or open an audio output. Check the VLC plugins installation.".to_owned()
                } else {
                    format!("VLC reported a playback error: {error}")
                };
                publish_player_position(engine, &state.queue, &snapshot, &message);
                publish(&snapshot, |state| state.state = PlaybackUiState::Error);
            }
            player_state => publish_player_state(engine, player_state, &state.queue, &snapshot),
        }
        refresh_media_snapshot(engine, &state.queue, &snapshot);
    }

    drop(engine);
}

fn handle_command(
    command: PlaybackCommand,
    engine: &mut Option<VlcEngine>,
    worker: &mut WorkerState,
    snapshot: &Mutex<PlaybackSnapshot>,
    frames: &FrameMailbox,
    runtime_error: &str,
) {
    match command {
        PlaybackCommand::PlayQueue {
            tracks,
            start_index,
            resume_position_ms,
        } => {
            worker.generation = worker.generation.wrapping_add(1);
            worker.ended_generation = None;
            cancel_pending_seek(
                &mut worker.pending_seek,
                snapshot,
                "Seek was canceled because the playback item changed.",
            );
            worker.queue_revision = worker.queue_revision.wrapping_add(1);
            worker.load_generation = worker.load_generation.wrapping_add(1);
            if worker.queue.replace(tracks, start_index) {
                publish_queue_snapshot(&worker.queue, worker.queue_revision, snapshot);
                if let Some(engine) = engine.as_mut() {
                    let started = start_current(
                        engine,
                        &worker.queue,
                        worker.queue_revision,
                        worker.load_generation,
                        snapshot,
                        worker.playback_rate,
                    );
                    if started {
                        bind_resume_seek(worker, resume_position_ms);
                    }
                } else {
                    publish_unavailable_current(
                        &worker.queue,
                        worker.queue_revision,
                        worker.load_generation,
                        snapshot,
                        runtime_error,
                    );
                }
            } else {
                if let Some(engine) = engine.as_mut() {
                    engine.stop_and_clear();
                } else {
                    frames.reset();
                }
                publish_empty_queue(
                    &worker.queue,
                    worker.queue_revision,
                    worker.load_generation,
                    snapshot,
                    if engine.is_some() {
                        PlaybackUiState::Idle
                    } else {
                        PlaybackUiState::Unavailable
                    },
                    "The queue contains no playable tracks.",
                );
            }
        }
        PlaybackCommand::TogglePause => {
            let Some(engine) = engine.as_mut() else {
                publish(snapshot, |state| state.message = runtime_error.to_owned());
                return;
            };
            if worker.queue.current().is_none() {
                publish(snapshot, |state| {
                    state.message = "Choose a track before using Play/Pause.".to_owned();
                });
            } else {
                let replaying_ended = engine.state() == PlayerState::Ended;
                if replaying_ended {
                    prepare_replay(worker, snapshot);
                    start_current(
                        engine,
                        &worker.queue,
                        worker.queue_revision,
                        worker.load_generation,
                        snapshot,
                        worker.playback_rate,
                    );
                } else {
                    worker.generation = worker.generation.wrapping_add(1);
                    worker.ended_generation = None;

                    match engine.toggle_pause() {
                        Ok(()) => publish_player_position(engine, &worker.queue, snapshot, ""),
                        Err(error) => {
                            publish(snapshot, |state| state.message = error.to_string());
                        }
                    }
                }
            }
        }
        PlaybackCommand::Stop => {
            let Some(engine) = engine.as_mut() else {
                publish(snapshot, |state| state.message = runtime_error.to_owned());
                return;
            };
            cancel_pending_seek(
                &mut worker.pending_seek,
                snapshot,
                "Seek was canceled because playback stopped.",
            );
            if worker.queue.current().is_some() {
                worker.generation = worker.generation.wrapping_add(1);
                worker.ended_generation = Some(worker.generation);
                engine.stop();
                publish(snapshot, |state| {
                    state.state = PlaybackUiState::Idle;
                    state.position_ms = 0;
                    state.duration_ms = 0;
                    state.message = "Playback stopped.".to_owned();
                });
            } else {
                publish(snapshot, |state| {
                    state.message = "Choose a track before using Stop.".to_owned();
                });
            }
        }
        PlaybackCommand::SeekTo {
            request_id,
            track_id,
            load_generation,
            position_ms,
        } => {
            let Some(engine) = engine.as_mut() else {
                publish(snapshot, |state| {
                    state.message = runtime_error.to_owned();
                    state.seek_ack = acknowledgement(request_id, false);
                });
                return;
            };
            if !current_media_matches(
                track_id,
                load_generation,
                &worker.queue,
                worker.load_generation,
                snapshot,
            ) {
                cancel_pending_seek(
                    &mut worker.pending_seek,
                    snapshot,
                    "Seek was canceled because the current media changed.",
                );
                publish(snapshot, |state| {
                    state.message =
                        "Seek was canceled because the current media changed.".to_owned();
                    state.seek_ack = acknowledgement(request_id, false);
                });
            } else if matches!(
                engine.state(),
                PlayerState::Opening | PlayerState::Buffering
            ) {
                cancel_pending_seek(
                    &mut worker.pending_seek,
                    snapshot,
                    "A newer seek replaced the pending seek.",
                );
                worker.pending_seek =
                    Some((request_id, track_id, load_generation, position_ms, None));
                publish(snapshot, |state| {
                    state.message = "Seeking when the track is ready.".to_owned();
                });
            } else {
                worker.pending_seek = None;
                apply_seek(engine, request_id, position_ms, &worker.queue, snapshot);
            }
        }
        PlaybackCommand::SetVolume {
            request_id,
            volume_percent,
        } => {
            let result = engine
                .as_ref()
                .ok_or_else(|| runtime_error.to_owned())
                .and_then(|engine| {
                    engine
                        .set_volume(volume_percent)
                        .map_err(|error| error.to_string())
                });
            publish(snapshot, |state| match result {
                Ok(()) => {
                    state.volume_percent = volume_percent;
                    state.message.clear();
                    state.volume_ack = acknowledgement(request_id, true);
                }
                Err(message) => {
                    state.message = message;
                    state.volume_ack = acknowledgement(request_id, false);
                }
            });
        }
        PlaybackCommand::SetPlaybackRate { request_id, rate } => {
            if worker.queue.current().is_none() {
                worker.playback_rate = rate;
                publish(snapshot, |state| {
                    state.playback_rate = rate;
                    state.message.clear();
                    state.playback_rate_ack = acknowledgement(request_id, true);
                });
                return;
            }
            let result = engine
                .as_ref()
                .ok_or_else(|| runtime_error.to_owned())
                .and_then(|engine| {
                    engine
                        .set_playback_rate(rate)
                        .map_err(|error| error.to_string())
                });
            publish(snapshot, |state| match result {
                Ok(()) => {
                    worker.playback_rate = rate;
                    state.playback_rate = rate;
                    state.message.clear();
                    state.playback_rate_ack = acknowledgement(request_id, true);
                }
                Err(message) => {
                    state.message = message;
                    state.playback_rate_ack = acknowledgement(request_id, false);
                }
            });
        }
        PlaybackCommand::Previous => {
            if worker.queue.previous().is_some() {
                worker.generation = worker.generation.wrapping_add(1);
                worker.load_generation = worker.load_generation.wrapping_add(1);
                worker.ended_generation = None;
                cancel_pending_seek(
                    &mut worker.pending_seek,
                    snapshot,
                    "Seek was canceled because the playback item changed.",
                );
                if let Some(engine) = engine.as_mut() {
                    start_current(
                        engine,
                        &worker.queue,
                        worker.queue_revision,
                        worker.load_generation,
                        snapshot,
                        worker.playback_rate,
                    );
                } else {
                    publish_unavailable_current(
                        &worker.queue,
                        worker.queue_revision,
                        worker.load_generation,
                        snapshot,
                        runtime_error,
                    );
                }
            } else {
                publish(snapshot, |state| {
                    state.message = if engine.is_some() {
                        "Choose a track before using Previous.".to_owned()
                    } else {
                        runtime_error.to_owned()
                    };
                });
            }
        }
        PlaybackCommand::Next => {
            if worker.queue.advance_next().is_some() {
                worker.generation = worker.generation.wrapping_add(1);
                worker.load_generation = worker.load_generation.wrapping_add(1);
                worker.ended_generation = None;
                cancel_pending_seek(
                    &mut worker.pending_seek,
                    snapshot,
                    "Seek was canceled because the playback item changed.",
                );
                if let Some(engine) = engine.as_mut() {
                    start_current(
                        engine,
                        &worker.queue,
                        worker.queue_revision,
                        worker.load_generation,
                        snapshot,
                        worker.playback_rate,
                    );
                } else {
                    publish_unavailable_current(
                        &worker.queue,
                        worker.queue_revision,
                        worker.load_generation,
                        snapshot,
                        runtime_error,
                    );
                }
            } else {
                publish(snapshot, |state| {
                    state.message = if engine.is_none() {
                        runtime_error.to_owned()
                    } else if worker.queue.is_empty() {
                        "Choose a track before using Next.".to_owned()
                    } else {
                        "This is the last track in the queue.".to_owned()
                    };
                });
            }
        }
        PlaybackCommand::SetRepeat(repeat_mode) => {
            worker.queue.set_repeat(repeat_mode);
            publish_queue_snapshot(&worker.queue, worker.queue_revision, snapshot);
        }
        PlaybackCommand::SetShuffle(shuffle) => {
            if worker.queue.is_shuffled() != shuffle {
                worker.queue.set_shuffle(shuffle);
                worker.queue_revision = worker.queue_revision.wrapping_add(1);
            }
            publish_queue_snapshot(&worker.queue, worker.queue_revision, snapshot);
        }
        PlaybackCommand::SelectAudioTrack {
            media_id,
            load_generation,
            track_id,
        } => {
            if !current_media_matches(
                media_id,
                load_generation,
                &worker.queue,
                worker.load_generation,
                snapshot,
            ) {
                return;
            }
            let result = engine
                .as_ref()
                .ok_or_else(|| runtime_error.to_owned())
                .and_then(|engine| {
                    let tracks = engine.media_controls().audio_tracks;
                    if !tracks.iter().any(|track| track.id == track_id) {
                        return Err(
                            "That audio track is not available for the current media.".to_owned()
                        );
                    }
                    engine
                        .select_audio_track(track_id)
                        .map_err(|error| error.to_string())
                });
            publish_control_result(snapshot, result);
        }
        PlaybackCommand::SelectSubtitleTrack {
            media_id,
            load_generation,
            track_id,
        } => {
            if !current_media_matches(
                media_id,
                load_generation,
                &worker.queue,
                worker.load_generation,
                snapshot,
            ) {
                return;
            }
            let result = engine
                .as_ref()
                .ok_or_else(|| runtime_error.to_owned())
                .and_then(|engine| {
                    if track_id != -1
                        && !engine
                            .media_controls()
                            .subtitle_tracks
                            .iter()
                            .any(|track| track.id == track_id)
                    {
                        return Err(
                            "That subtitle track is not available for the current media."
                                .to_owned(),
                        );
                    }
                    engine
                        .select_subtitle_track(track_id)
                        .map_err(|error| error.to_string())
                });
            publish_control_result(snapshot, result);
        }
        PlaybackCommand::AddSubtitle {
            media_id,
            load_generation,
            path,
        } => {
            if !current_media_matches(
                media_id,
                load_generation,
                &worker.queue,
                worker.load_generation,
                snapshot,
            ) {
                return;
            }
            let result = engine
                .as_ref()
                .ok_or_else(|| runtime_error.to_owned())
                .and_then(|engine| {
                    engine
                        .add_subtitle(&path)
                        .map_err(|error| error.to_string())
                });
            publish_control_result(snapshot, result);
        }
        PlaybackCommand::SetAudioDelay {
            media_id,
            load_generation,
            delay_us,
        } => {
            if !current_media_matches(
                media_id,
                load_generation,
                &worker.queue,
                worker.load_generation,
                snapshot,
            ) {
                return;
            }
            let result = engine
                .as_ref()
                .ok_or_else(|| runtime_error.to_owned())
                .and_then(|engine| {
                    engine
                        .set_audio_delay(delay_us)
                        .map_err(|error| error.to_string())
                });
            publish_control_result(snapshot, result);
        }
        PlaybackCommand::SetSubtitleDelay {
            media_id,
            load_generation,
            delay_us,
        } => {
            if !current_media_matches(
                media_id,
                load_generation,
                &worker.queue,
                worker.load_generation,
                snapshot,
            ) {
                return;
            }
            let result = engine
                .as_ref()
                .ok_or_else(|| runtime_error.to_owned())
                .and_then(|engine| {
                    engine
                        .set_subtitle_delay(delay_us)
                        .map_err(|error| error.to_string())
                });
            publish_control_result(snapshot, result);
        }
        PlaybackCommand::SelectChapter {
            media_id,
            load_generation,
            chapter,
        } => {
            if !current_media_matches(
                media_id,
                load_generation,
                &worker.queue,
                worker.load_generation,
                snapshot,
            ) {
                return;
            }
            let result = engine
                .as_ref()
                .ok_or_else(|| runtime_error.to_owned())
                .and_then(|engine| {
                    engine
                        .select_chapter(chapter)
                        .map_err(|error| error.to_string())
                });
            publish_control_result(snapshot, result);
        }
        PlaybackCommand::QueueJump { revision, position } => {
            match queue_jump(&mut worker.queue, revision, worker.queue_revision, position) {
                Ok(()) => {
                    worker.generation = worker.generation.wrapping_add(1);
                    worker.load_generation = worker.load_generation.wrapping_add(1);
                    worker.ended_generation = None;
                    cancel_pending_seek(
                        &mut worker.pending_seek,
                        snapshot,
                        "Seek was canceled because the playback item changed.",
                    );
                    if let Some(engine) = engine.as_mut() {
                        start_current(
                            engine,
                            &worker.queue,
                            worker.queue_revision,
                            worker.load_generation,
                            snapshot,
                            worker.playback_rate,
                        );
                    } else {
                        publish_unavailable_current(
                            &worker.queue,
                            worker.queue_revision,
                            worker.load_generation,
                            snapshot,
                            runtime_error,
                        );
                    }
                }
                Err(message) => publish(snapshot, |state| state.message = message),
            }
        }
        PlaybackCommand::QueueMove { revision, from, to } => {
            match queue_move(
                &mut worker.queue,
                &mut worker.queue_revision,
                revision,
                from,
                to,
            ) {
                Ok(_) => publish_queue_snapshot(&worker.queue, worker.queue_revision, snapshot),
                Err(message) => publish(snapshot, |state| state.message = message),
            }
        }
        PlaybackCommand::QueueRemove { revision, position } => {
            let was_paused = engine
                .as_ref()
                .is_some_and(|engine| engine.state() == PlayerState::Paused);
            match queue_remove(
                &mut worker.queue,
                &mut worker.queue_revision,
                revision,
                position,
            ) {
                Ok(removed_current) => {
                    publish_queue_snapshot(&worker.queue, worker.queue_revision, snapshot);
                    if removed_current {
                        worker.generation = worker.generation.wrapping_add(1);
                        worker.load_generation = worker.load_generation.wrapping_add(1);
                        worker.ended_generation = None;
                        cancel_pending_seek(
                            &mut worker.pending_seek,
                            snapshot,
                            "Seek was canceled because the playback item changed.",
                        );
                        if worker.queue.current().is_some() {
                            if let Some(engine) = engine.as_mut() {
                                start_current(
                                    engine,
                                    &worker.queue,
                                    worker.queue_revision,
                                    worker.load_generation,
                                    snapshot,
                                    worker.playback_rate,
                                );
                                if was_paused {
                                    engine.set_paused(true);
                                }
                            } else {
                                publish_unavailable_current(
                                    &worker.queue,
                                    worker.queue_revision,
                                    worker.load_generation,
                                    snapshot,
                                    runtime_error,
                                );
                            }
                        } else {
                            if let Some(engine) = engine.as_mut() {
                                engine.stop_and_clear();
                            } else {
                                frames.reset();
                            }
                            publish_empty_queue(
                                &worker.queue,
                                worker.queue_revision,
                                worker.load_generation,
                                snapshot,
                                if engine.is_some() {
                                    PlaybackUiState::Idle
                                } else {
                                    PlaybackUiState::Unavailable
                                },
                                "The playback queue is empty.",
                            );
                        }
                    }
                }
                Err(message) => publish(snapshot, |state| state.message = message),
            }
        }
        PlaybackCommand::RefreshOwningQueue { revision, tracks } => {
            match refresh_owning_queue(
                &mut worker.queue,
                &mut worker.queue_revision,
                revision,
                tracks,
            ) {
                Ok(changed) => {
                    publish_queue_snapshot(&worker.queue, worker.queue_revision, snapshot);
                    publish(snapshot, |state| {
                        state.message = if changed {
                            "Updated the active playlist queue.".to_owned()
                        } else {
                            "The active playlist queue already matches the playlist.".to_owned()
                        };
                    });
                }
                Err(message) => publish(snapshot, |state| state.message = message),
            }
        }
        PlaybackCommand::PlayNext { track } => {
            let was_empty = worker.queue.insert_next(track);
            worker.queue_revision = worker.queue_revision.wrapping_add(1);
            publish_queue_snapshot(&worker.queue, worker.queue_revision, snapshot);
            if was_empty {
                worker.generation = worker.generation.wrapping_add(1);
                worker.load_generation = worker.load_generation.wrapping_add(1);
                worker.ended_generation = None;
                if let Some(engine) = engine.as_mut() {
                    start_current(
                        engine,
                        &worker.queue,
                        worker.queue_revision,
                        worker.load_generation,
                        snapshot,
                        worker.playback_rate,
                    );
                } else {
                    publish_unavailable_current(
                        &worker.queue,
                        worker.queue_revision,
                        worker.load_generation,
                        snapshot,
                        runtime_error,
                    );
                }
            }
        }
        PlaybackCommand::Shutdown => {}
    }
}

fn current_media_matches(
    media_id: TrackId,
    load_generation: u64,
    queue: &PlaybackQueue,
    current_load_generation: u64,
    snapshot: &Mutex<PlaybackSnapshot>,
) -> bool {
    if queue.current().map(|track| track.id) == Some(media_id)
        && load_generation == current_load_generation
    {
        true
    } else {
        publish(snapshot, |state| {
            state.message =
                "This control was canceled because the current media changed.".to_owned();
        });
        false
    }
}

fn publish_control_result(snapshot: &Mutex<PlaybackSnapshot>, result: Result<(), String>) {
    publish(snapshot, |state| match result {
        Ok(()) => state.message.clear(),
        Err(message) => state.message = message,
    });
}

fn publish_queue_snapshot(
    queue: &PlaybackQueue,
    revision: u64,
    snapshot: &Mutex<PlaybackSnapshot>,
) {
    let queue_titles = queue
        .ordered_entries()
        .into_iter()
        .map(|track| track.title.clone())
        .collect();
    publish(snapshot, |state| {
        state.queue_index = queue
            .traversal_position()
            .map_or(0, |position| position + 1);
        state.queue_len = queue.len();
        state.queue_titles = queue_titles;
        state.queue_revision = revision;
        state.repeat_mode = queue.repeat_mode();
        state.shuffle = queue.is_shuffled();
    });
}

fn clear_media_snapshot(snapshot: &Mutex<PlaybackSnapshot>) {
    publish(snapshot, |state| {
        state.audio_tracks.clear();
        state.subtitle_tracks.clear();
        state.chapters.clear();
        state.audio_track = -1;
        state.subtitle_track = -1;
        state.chapter = -1;
        state.audio_delay_us = 0;
        state.subtitle_delay_us = 0;
    });
}

fn publish_unavailable_current(
    queue: &PlaybackQueue,
    revision: u64,
    load_generation: u64,
    snapshot: &Mutex<PlaybackSnapshot>,
    runtime_error: &str,
) {
    publish_queue_snapshot(queue, revision, snapshot);
    clear_media_snapshot(snapshot);
    publish(snapshot, |state| {
        state.load_generation = load_generation;
        if let Some(track) = queue.current() {
            state.current_id = Some(track.id);
            state.current_is_video = is_movie(track);
            state.current_title = track.title.clone();
        }
        state.position_ms = 0;
        state.duration_ms = 0;
        state.state = PlaybackUiState::Unavailable;
        state.message = runtime_error.to_owned();
    });
}

fn publish_empty_queue(
    queue: &PlaybackQueue,
    revision: u64,
    load_generation: u64,
    snapshot: &Mutex<PlaybackSnapshot>,
    state_value: PlaybackUiState,
    message: &str,
) {
    publish_queue_snapshot(queue, revision, snapshot);
    clear_media_snapshot(snapshot);
    publish(snapshot, |state| {
        state.load_generation = load_generation;
        state.current_id = None;
        state.current_is_video = false;
        state.current_title.clear();
        state.position_ms = 0;
        state.duration_ms = 0;
        state.state = state_value;
        state.message = message.to_owned();
    });
}

fn refresh_media_snapshot(
    engine: &VlcEngine,
    queue: &PlaybackQueue,
    snapshot: &Mutex<PlaybackSnapshot>,
) {
    let Some(track) = queue.current() else {
        clear_media_snapshot(snapshot);
        return;
    };
    let controls = engine.media_controls();
    publish(snapshot, |state| {
        state.current_id = Some(track.id);
        state.current_is_video = is_movie(track);
        state.current_title = track.title.clone();
        state.audio_tracks = controls.audio_tracks;
        state.subtitle_tracks = controls.subtitle_tracks;
        state.chapters = controls.chapters;
        state.audio_track = controls.audio_track;
        state.subtitle_track = controls.subtitle_track;
        state.chapter = controls.chapter;
        state.audio_delay_us = controls.audio_delay_us;
        state.subtitle_delay_us = controls.subtitle_delay_us;
    });
}

fn queue_jump(
    queue: &mut PlaybackQueue,
    expected_revision: u64,
    actual_revision: u64,
    position: usize,
) -> Result<(), String> {
    ensure_queue_revision(expected_revision, actual_revision)?;
    queue
        .select_at(position)
        .then_some(())
        .ok_or_else(|| "That queue position is no longer available.".to_owned())
}

fn queue_move(
    queue: &mut PlaybackQueue,
    revision: &mut u64,
    expected_revision: u64,
    from: usize,
    to: usize,
) -> Result<bool, String> {
    ensure_queue_revision(expected_revision, *revision)?;
    if !queue.move_entry(from, to) {
        return Err("That queue position is no longer available.".to_owned());
    }
    let changed = from != to;
    if changed {
        *revision = revision.wrapping_add(1);
    }
    Ok(changed)
}

fn queue_remove(
    queue: &mut PlaybackQueue,
    revision: &mut u64,
    expected_revision: u64,
    position: usize,
) -> Result<bool, String> {
    ensure_queue_revision(expected_revision, *revision)?;
    let removed_current = queue
        .remove_at(position)
        .ok_or_else(|| "That queue position is no longer available.".to_owned())?;
    *revision = revision.wrapping_add(1);
    Ok(removed_current)
}

fn refresh_owning_queue(
    queue: &mut PlaybackQueue,
    revision: &mut u64,
    expected_revision: u64,
    tracks: Vec<Track>,
) -> Result<bool, String> {
    ensure_queue_revision(expected_revision, *revision)?;
    let changed = queue
        .reorder_occurrences(tracks)
        .map_err(|()| "The active queue no longer matches that playlist.".to_owned())?;
    if changed {
        *revision = revision.wrapping_add(1);
    }
    Ok(changed)
}

fn ensure_queue_revision(expected_revision: u64, actual_revision: u64) -> Result<(), String> {
    if expected_revision == actual_revision {
        Ok(())
    } else {
        Err("The queue changed. Refresh it and try again.".to_owned())
    }
}

fn start_current(
    engine: &mut VlcEngine,
    queue: &PlaybackQueue,
    queue_revision: u64,
    load_generation: u64,
    snapshot: &Mutex<PlaybackSnapshot>,
    playback_rate: f32,
) -> bool {
    let Some(track) = queue.current().cloned() else {
        return false;
    };

    engine.stop_and_clear();
    publish_queue_snapshot(queue, queue_revision, snapshot);
    clear_media_snapshot(snapshot);
    publish(snapshot, |state| {
        state.load_generation = load_generation;
        state.current_id = Some(track.id);
        state.current_is_video = is_movie(&track);
        state.current_title = track.title.clone();
        state.queue_index = queue
            .traversal_position()
            .map_or(0, |position| position + 1);
        state.state = PlaybackUiState::Loading;
        state.position_ms = 0;
        state.duration_ms = 0;
        state.playback_rate = 1.0;
        state.message = "Opening media file…".to_owned();
    });

    match engine.play_after_stop(&track) {
        Err(error) => {
            publish(snapshot, |state| {
                state.state = PlaybackUiState::Error;
                state.message = error.to_string();
            });
            false
        }
        Ok(()) => {
            if let Err(error) = engine.set_playback_rate(playback_rate) {
                publish(snapshot, |state| state.message = error.to_string());
            } else {
                publish(snapshot, |state| state.playback_rate = playback_rate);
            }
            true
        }
    }
}

fn apply_seek(
    engine: &VlcEngine,
    request_id: u64,
    position_ms: i64,
    queue: &PlaybackQueue,
    snapshot: &Mutex<PlaybackSnapshot>,
) {
    let duration_ms = engine.length_ms();
    match engine.seek(position_ms, duration_ms) {
        Ok(position_ms) => {
            publish_player_position(engine, queue, snapshot, "");
            publish(snapshot, |state| {
                state.position_ms = position_ms;
                state.message.clear();
                state.seek_ack = acknowledgement(request_id, true);
            });
        }
        Err(error) => publish(snapshot, |state| {
            state.message = error.to_string();
            state.seek_ack = acknowledgement(request_id, false);
        }),
    }
}

fn publish_player_position(
    engine: &VlcEngine,
    queue: &PlaybackQueue,
    snapshot: &Mutex<PlaybackSnapshot>,
    message: &str,
) {
    let position_ms = engine.time_ms().max(0);
    let duration_ms = engine.length_ms().max(0);
    publish(snapshot, |state| {
        state.current_id = queue.current().map(|track| track.id);
        state.current_is_video = queue.current().is_some_and(is_movie);
        state.current_title = queue
            .current()
            .map(|track| track.title.clone())
            .unwrap_or_default();
        state.queue_index = queue
            .traversal_position()
            .map_or(0, |position| position + 1);
        state.queue_len = queue.len();
        state.position_ms = position_ms;
        state.duration_ms = duration_ms;
        if !message.is_empty() {
            state.message = message.to_owned();
        }
    });
}

fn publish_player_state(
    engine: &VlcEngine,
    state: PlayerState,
    queue: &PlaybackQueue,
    snapshot: &Mutex<PlaybackSnapshot>,
) {
    let position_ms = engine.time_ms().max(0);
    let duration_ms = engine.length_ms().max(0);
    publish(snapshot, |current| {
        current.current_id = queue.current().map(|track| track.id);
        current.current_is_video = queue.current().is_some_and(is_movie);
        current.current_title = queue
            .current()
            .map(|track| track.title.clone())
            .unwrap_or_default();
        current.queue_index = queue
            .traversal_position()
            .map_or(0, |position| position + 1);
        current.queue_len = queue.len();
        current.position_ms = position_ms;
        current.duration_ms = duration_ms;
        current.state = match state {
            PlayerState::Playing => PlaybackUiState::Playing,
            PlayerState::Paused => PlaybackUiState::Paused,
            PlayerState::Opening | PlayerState::Buffering => PlaybackUiState::Loading,
            PlayerState::Ended => PlaybackUiState::Ended,
            PlayerState::Error => PlaybackUiState::Error,
            PlayerState::Stopped | PlayerState::Other => PlaybackUiState::Idle,
        };
        if matches!(state, PlayerState::Playing | PlayerState::Paused)
            && current.message == "Opening media file…"
        {
            current.message.clear();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{
        bind_resume_seek, cancel_pending_seek, current_media_matches, ensure_queue_revision,
        handle_command, pending_seek_waits_for_ready, prepare_replay, publish_queue_snapshot,
        queue_jump, queue_move, queue_remove, seek_target_ms, send_command, valid_media_delay,
        valid_playback_rate, valid_volume_percent, PlaybackCommand, PlaybackSnapshot,
        PlaybackUiState, PlayerState, WorkerState, MAX_MEDIA_DELAY_US, MAX_PLAYBACK_RATE,
        MAX_VOLUME_PERCENT, MIN_PLAYBACK_RATE, MIN_VOLUME_PERCENT, RESUME_SEEK_READY_TIMEOUT,
        RESUME_SEEK_REQUEST_ID,
    };
    use crate::model::Track;
    use crate::playback::queue::{PlaybackQueue, RepeatMode};
    use std::path::PathBuf;
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::Instant;

    fn track(id: i32) -> Track {
        Track {
            id,
            path: PathBuf::from(format!("track-{id}.mp4")),
            title: format!("Track {id}"),
            format: "MP4".to_owned(),
        }
    }

    #[test]
    fn control_values_are_validated_and_seek_targets_are_clamped() {
        assert!(valid_volume_percent(MIN_VOLUME_PERCENT));
        assert!(valid_volume_percent(MAX_VOLUME_PERCENT));
        assert!(!valid_volume_percent(MIN_VOLUME_PERCENT - 1));
        assert!(!valid_volume_percent(MAX_VOLUME_PERCENT + 1));

        assert!(valid_playback_rate(MIN_PLAYBACK_RATE));
        assert!(valid_playback_rate(MAX_PLAYBACK_RATE));
        assert!(!valid_playback_rate(MIN_PLAYBACK_RATE - 0.01));
        assert!(!valid_playback_rate(MAX_PLAYBACK_RATE + 0.01));
        assert!(!valid_playback_rate(f32::NAN));
        assert!(!valid_playback_rate(f32::INFINITY));

        assert_eq!(seek_target_ms(0, 5_000), Some(0));
        assert_eq!(seek_target_ms(1_200, 5_000), Some(1_200));
        assert_eq!(seek_target_ms(8_000, 5_000), Some(5_000));
        assert_eq!(seek_target_ms(-1, 5_000), None);
        assert_eq!(seek_target_ms(1, 0), None);
    }

    #[test]
    fn invalid_controls_do_not_enter_the_bounded_command_queue() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let snapshot = Arc::new(Mutex::new(PlaybackSnapshot::default()));

        assert!(send_command(
            &sender,
            PlaybackCommand::SeekTo {
                request_id: 1,
                track_id: 1,
                load_generation: 0,
                position_ms: -1,
            },
            &snapshot
        )
        .is_err());
        assert!(send_command(
            &sender,
            PlaybackCommand::PlayQueue {
                tracks: vec![track(1)],
                start_index: 0,
                resume_position_ms: Some(-1),
            },
            &snapshot
        )
        .is_err());
        assert!(send_command(
            &sender,
            PlaybackCommand::SetVolume {
                request_id: 2,
                volume_percent: 101,
            },
            &snapshot
        )
        .is_err());
        assert!(send_command(
            &sender,
            PlaybackCommand::SetPlaybackRate {
                request_id: 3,
                rate: f32::NAN,
            },
            &snapshot
        )
        .is_err());

        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        let snapshot = snapshot.lock().expect("snapshot should not be poisoned");
        assert!(snapshot.message.contains("Playback rate"));
    }

    #[test]
    fn media_controls_reject_stale_ids_and_out_of_range_delays() {
        let queue = {
            let mut queue = PlaybackQueue::default();
            assert!(queue.replace(vec![track(1)], 0));
            queue
        };
        let snapshot = Mutex::new(PlaybackSnapshot::default());
        assert!(!current_media_matches(2, 0, &queue, 0, &snapshot));
        assert!(snapshot
            .lock()
            .unwrap()
            .message
            .contains("current media changed"));

        assert!(valid_media_delay(-MAX_MEDIA_DELAY_US));
        assert!(valid_media_delay(MAX_MEDIA_DELAY_US));
        assert!(!valid_media_delay(MAX_MEDIA_DELAY_US + 1));
        assert!(!valid_media_delay(-MAX_MEDIA_DELAY_US - 1));
    }

    #[test]
    fn media_controls_reject_stale_generation_even_for_the_same_track_id() {
        let mut queue = PlaybackQueue::default();
        assert!(queue.replace(vec![track(1), track(1)], 1));
        let snapshot = Mutex::new(PlaybackSnapshot::default());

        assert!(!current_media_matches(1, 8, &queue, 9, &snapshot));
        assert!(snapshot
            .lock()
            .unwrap()
            .message
            .contains("current media changed"));
        assert!(current_media_matches(1, 9, &queue, 9, &snapshot));
    }

    #[test]
    fn replay_from_ended_starts_a_new_load_and_cancels_pending_seek() {
        let mut worker = WorkerState {
            generation: 4,
            load_generation: 12,
            ended_generation: Some(4),
            pending_seek: Some((7, 1, 12, 2_500, None)),
            ..WorkerState::default()
        };
        let snapshot = Mutex::new(PlaybackSnapshot {
            load_generation: 12,
            ..PlaybackSnapshot::default()
        });

        prepare_replay(&mut worker, &snapshot);

        assert_eq!(worker.generation, 5);
        assert_eq!(worker.load_generation, 13);
        assert_eq!(worker.ended_generation, None);
        assert_eq!(worker.pending_seek, None);
        let snapshot = snapshot.lock().unwrap();
        assert_eq!(snapshot.load_generation, 13);
        assert_eq!(snapshot.seek_ack.request_id, 7);
        assert!(!snapshot.seek_ack.succeeded);
        assert!(snapshot.message.contains("playback restarted"));
    }

    #[test]
    fn resumed_queue_seek_uses_new_generation_for_same_track_and_stop_cancels_it() {
        let mut worker = WorkerState {
            load_generation: 13,
            ..WorkerState::default()
        };
        assert!(worker.queue.replace(vec![track(7), track(7)], 1));
        let snapshot = Mutex::new(PlaybackSnapshot {
            current_id: Some(7),
            load_generation: 13,
            ..PlaybackSnapshot::default()
        });

        bind_resume_seek(&mut worker, Some(12_000));

        let (request_id, track_id, load_generation, position_ms, deadline) =
            worker.pending_seek.expect("resume seek should be pending");
        assert_eq!(
            (request_id, track_id, load_generation, position_ms),
            (RESUME_SEEK_REQUEST_ID, 7, 13, 12_000)
        );
        assert!(deadline.is_some_and(|deadline| deadline > Instant::now()));
        assert!(!current_media_matches(
            7,
            12,
            &worker.queue,
            worker.load_generation,
            &snapshot
        ));
        assert!(current_media_matches(
            7,
            13,
            &worker.queue,
            worker.load_generation,
            &snapshot
        ));

        cancel_pending_seek(
            &mut worker.pending_seek,
            &snapshot,
            "Seek was canceled because playback stopped.",
        );
        assert_eq!(worker.pending_seek, None);
        let snapshot = snapshot.lock().expect("snapshot should not be poisoned");
        assert_eq!(snapshot.seek_ack.request_id, RESUME_SEEK_REQUEST_ID);
        assert!(!snapshot.seek_ack.succeeded);
    }

    #[test]
    fn resume_seek_waits_for_startup_readiness_until_its_deadline() {
        let now = Instant::now();
        let deadline = now + RESUME_SEEK_READY_TIMEOUT;
        for state in [
            PlayerState::Opening,
            PlayerState::Buffering,
            PlayerState::Stopped,
            PlayerState::Other,
        ] {
            assert!(pending_seek_waits_for_ready(
                RESUME_SEEK_REQUEST_ID,
                state,
                0,
                Some(deadline),
                now
            ));
        }
        for state in [PlayerState::Playing, PlayerState::Paused] {
            assert!(pending_seek_waits_for_ready(
                RESUME_SEEK_REQUEST_ID,
                state,
                0,
                Some(deadline),
                now
            ));
            assert!(!pending_seek_waits_for_ready(
                RESUME_SEEK_REQUEST_ID,
                state,
                60_000,
                Some(deadline),
                now
            ));
        }
        for state in [PlayerState::Ended, PlayerState::Error] {
            assert!(!pending_seek_waits_for_ready(
                RESUME_SEEK_REQUEST_ID,
                state,
                0,
                Some(deadline),
                now
            ));
        }
        assert!(!pending_seek_waits_for_ready(
            RESUME_SEEK_REQUEST_ID,
            PlayerState::Opening,
            0,
            Some(deadline),
            deadline
        ));
        assert!(!pending_seek_waits_for_ready(
            RESUME_SEEK_REQUEST_ID,
            PlayerState::Opening,
            0,
            None,
            now
        ));
        assert!(pending_seek_waits_for_ready(
            1,
            PlayerState::Opening,
            0,
            None,
            now
        ));
        assert!(!pending_seek_waits_for_ready(
            1,
            PlayerState::Other,
            0,
            None,
            now
        ));
    }

    #[test]
    fn queue_commands_guard_revisions_and_preserve_current_occurrence() {
        let mut queue = PlaybackQueue::default();
        assert!(queue.replace(vec![track(1), track(2), track(3)], 1));
        let mut revision = 7;

        assert!(ensure_queue_revision(6, revision).is_err());
        assert!(queue_move(&mut queue, &mut revision, 6, 0, 2).is_err());
        assert_eq!(queue.current().unwrap().id, 2);
        assert_eq!(revision, 7);

        assert_eq!(queue_move(&mut queue, &mut revision, 7, 0, 2), Ok(true));
        assert_eq!(revision, 8);
        assert_eq!(queue.current().unwrap().id, 2);
        assert_eq!(queue.traversal_position(), Some(0));
        assert_eq!(queue_move(&mut queue, &mut revision, 8, 1, 1), Ok(false));
        assert_eq!(revision, 8);

        assert!(queue_remove(&mut queue, &mut revision, 7, 1).is_err());
        assert_eq!(queue.len(), 3);
        assert_eq!(queue_remove(&mut queue, &mut revision, 8, 0), Ok(true));
        assert_eq!(revision, 9);
        assert_eq!(queue.current().unwrap().id, 3);

        assert!(queue_jump(&mut queue, 8, revision, 1).is_err());
        assert_eq!(queue.current().unwrap().id, 3);
        assert!(queue_jump(&mut queue, 9, revision, 1).is_ok());
        assert_eq!(queue.current().unwrap().id, 1);
    }

    #[test]
    fn queue_snapshot_uses_traversal_order_and_position() {
        let mut queue = PlaybackQueue::default();
        queue.set_repeat(RepeatMode::All);
        queue.set_shuffle(true);
        assert!(queue.replace(vec![track(1), track(2), track(3)], 2));
        assert_eq!(queue.current_index(), Some(2));
        assert_eq!(queue.traversal_position(), Some(0));

        let snapshot = Mutex::new(PlaybackSnapshot::default());
        publish_queue_snapshot(&queue, 12, &snapshot);
        let snapshot = snapshot.lock().unwrap();
        assert_eq!(snapshot.queue_index, 1);
        assert_eq!(snapshot.queue_len, 3);
        assert_eq!(snapshot.queue_titles[0], "Track 3");
        assert_eq!(snapshot.queue_revision, 12);
        assert_eq!(snapshot.repeat_mode, RepeatMode::All);
        assert!(snapshot.shuffle);
    }

    #[test]
    fn owning_playlist_reorder_preserves_the_playing_duplicate_and_media_snapshot() {
        let duplicate = track(7);
        let mut worker = WorkerState::default();
        assert!(worker
            .queue
            .replace(vec![duplicate.clone(), track(8), duplicate.clone()], 2));
        worker.queue_revision = 4;
        worker.load_generation = 12;

        let snapshot = Mutex::new(PlaybackSnapshot {
            current_id: Some(7),
            load_generation: 12,
            current_title: "Track 7".to_owned(),
            state: PlaybackUiState::Paused,
            position_ms: 4_321,
            duration_ms: 12_000,
            ..PlaybackSnapshot::default()
        });
        let mut engine = None;
        let frames = crate::playback::frame::FrameMailbox::default();
        handle_command(
            PlaybackCommand::RefreshOwningQueue {
                revision: 4,
                tracks: vec![duplicate.clone(), duplicate.clone(), track(8)],
            },
            &mut engine,
            &mut worker,
            &snapshot,
            &frames,
            "runtime unavailable",
        );

        assert_eq!(worker.queue_revision, 5);
        assert_eq!(worker.queue.current_index(), Some(1));
        assert_eq!(worker.queue.current().unwrap().id, 7);
        let snapshot = snapshot.lock().unwrap();
        assert_eq!(snapshot.queue_titles, ["Track 7", "Track 7", "Track 8"]);
        assert_eq!(snapshot.queue_revision, 5);
        assert_eq!(snapshot.current_id, Some(7));
        assert_eq!(snapshot.current_title, "Track 7");
        assert_eq!(snapshot.load_generation, 12);
        assert_eq!(snapshot.state, PlaybackUiState::Paused);
        assert_eq!(snapshot.position_ms, 4_321);
        assert_eq!(snapshot.duration_ms, 12_000);
        assert!(snapshot
            .message
            .contains("Updated the active playlist queue"));
    }

    #[test]
    fn owning_playlist_reorder_rejects_stale_revision_and_different_contents() {
        let tracks = vec![track(1), track(2)];
        let mut worker = WorkerState::default();
        assert!(worker.queue.replace(tracks.clone(), 0));
        worker.queue_revision = 6;
        let mut engine = None;
        let frames = crate::playback::frame::FrameMailbox::default();
        let snapshot = Mutex::new(PlaybackSnapshot::default());

        handle_command(
            PlaybackCommand::RefreshOwningQueue {
                revision: 5,
                tracks: vec![track(2), track(1)],
            },
            &mut engine,
            &mut worker,
            &snapshot,
            &frames,
            "runtime unavailable",
        );
        assert_eq!(worker.queue_revision, 6);
        assert_eq!(worker.queue.ordered_entries()[0].id, 1);
        assert!(snapshot.lock().unwrap().message.contains("queue changed"));

        handle_command(
            PlaybackCommand::RefreshOwningQueue {
                revision: 6,
                tracks: vec![track(3), track(1)],
            },
            &mut engine,
            &mut worker,
            &snapshot,
            &frames,
            "runtime unavailable",
        );
        assert_eq!(worker.queue_revision, 6);
        assert_eq!(worker.queue.ordered_entries()[0].id, 1);
        assert!(snapshot
            .lock()
            .unwrap()
            .message
            .contains("no longer matches that playlist"));
    }

    #[test]
    fn invalid_media_commands_are_rejected_before_entering_queue() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let snapshot = Arc::new(Mutex::new(PlaybackSnapshot::default()));
        let missing_subtitle = PathBuf::from("missing-subtitle-file.srt");
        let invalid = [
            PlaybackCommand::SelectAudioTrack {
                media_id: 1,
                load_generation: 0,
                track_id: -1,
            },
            PlaybackCommand::SelectSubtitleTrack {
                media_id: 1,
                load_generation: 0,
                track_id: -2,
            },
            PlaybackCommand::AddSubtitle {
                media_id: 1,
                load_generation: 0,
                path: missing_subtitle,
            },
            PlaybackCommand::SetAudioDelay {
                media_id: 1,
                load_generation: 0,
                delay_us: MAX_MEDIA_DELAY_US + 1,
            },
            PlaybackCommand::SetSubtitleDelay {
                media_id: 1,
                load_generation: 0,
                delay_us: i64::MIN,
            },
            PlaybackCommand::SelectChapter {
                media_id: 1,
                load_generation: 0,
                chapter: -1,
            },
        ];
        for command in invalid {
            assert!(send_command(&sender, command, &snapshot).is_err());
        }
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }
}
