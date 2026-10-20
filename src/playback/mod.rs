mod ffi;
pub mod queue;

use crate::model::{Track, TrackId};
use crate::playback::ffi::{PlayerState, VlcEngine};
use crate::playback::queue::PlaybackQueue;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const COMMAND_CAPACITY: usize = 32;
const POLL_INTERVAL: Duration = Duration::from_millis(160);

#[derive(Clone, Debug, Default)]
pub struct PlaybackSnapshot {
    pub runtime_checked: bool,
    pub runtime_available: bool,
    pub runtime_message: String,
    pub current_id: Option<TrackId>,
    pub current_title: String,
    pub state: PlaybackUiState,
    pub queue_index: usize,
    pub queue_len: usize,
    pub position_ms: i64,
    pub duration_ms: i64,
    pub message: String,
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
    },
    TogglePause,
    Previous,
    Next,
    Shutdown,
}

pub struct PlaybackWorker {
    sender: SyncSender<PlaybackCommand>,
    snapshot: Arc<Mutex<PlaybackSnapshot>>,
    join: Option<JoinHandle<()>>,
}

impl PlaybackWorker {
    pub fn spawn() -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        let snapshot = Arc::new(Mutex::new(PlaybackSnapshot::default()));
        let worker_snapshot = Arc::clone(&snapshot);
        let join = thread::Builder::new()
            .name("vlc-playback-owner".to_owned())
            .spawn(move || run_worker(receiver, worker_snapshot))?;

        Ok(Self {
            sender,
            snapshot,
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

fn lock_snapshot(snapshot: &Mutex<PlaybackSnapshot>) -> MutexGuard<'_, PlaybackSnapshot> {
    snapshot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn publish(snapshot: &Mutex<PlaybackSnapshot>, update: impl FnOnce(&mut PlaybackSnapshot)) {
    update(&mut lock_snapshot(snapshot));
}

fn run_worker(receiver: Receiver<PlaybackCommand>, snapshot: Arc<Mutex<PlaybackSnapshot>>) {
    let mut engine = match VlcEngine::new() {
        Ok(engine) => {
            let runtime_message = format!("libVLC {} ready", engine.version());
            publish(&snapshot, |state| {
                state.runtime_checked = true;
                state.runtime_available = true;
                state.runtime_message = runtime_message;
                state.message = "Choose a local audio file to begin.".to_owned();
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
    let mut queue = PlaybackQueue::default();
    let mut ended_generation = None;
    let mut generation: u64 = 0;

    loop {
        match receiver.recv_timeout(POLL_INTERVAL) {
            Ok(PlaybackCommand::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(command) => {
                if let Some(engine) = engine.as_mut() {
                    match command {
                        PlaybackCommand::PlayQueue {
                            tracks,
                            start_index,
                        } => {
                            generation = generation.wrapping_add(1);
                            ended_generation = None;
                            if queue.replace(tracks, start_index) {
                                start_current(engine, &queue, &snapshot);
                            } else {
                                publish(&snapshot, |state| {
                                    state.state = PlaybackUiState::Idle;
                                    state.current_id = None;
                                    state.current_title.clear();
                                    state.queue_index = 0;
                                    state.queue_len = 0;
                                    state.position_ms = 0;
                                    state.duration_ms = 0;
                                    state.message =
                                        "The queue contains no playable tracks.".to_owned();
                                });
                            }
                        }
                        PlaybackCommand::TogglePause => {
                            engine.toggle_pause();
                            publish_player_position(engine, &queue, &snapshot, "");
                        }
                        PlaybackCommand::Previous => {
                            if queue.previous().is_some() {
                                generation = generation.wrapping_add(1);
                                ended_generation = None;
                                start_current(engine, &queue, &snapshot);
                            } else {
                                publish(&snapshot, |state| {
                                    state.message =
                                        "Choose a track before using Previous.".to_owned();
                                });
                            }
                        }
                        PlaybackCommand::Next => {
                            if queue.advance_next().is_some() {
                                generation = generation.wrapping_add(1);
                                ended_generation = None;
                                start_current(engine, &queue, &snapshot);
                            } else {
                                publish(&snapshot, |state| {
                                    state.message = if queue.is_empty() {
                                        "Choose a track before using Next.".to_owned()
                                    } else {
                                        "This is the last track in the queue.".to_owned()
                                    };
                                });
                            }
                        }
                        PlaybackCommand::Shutdown => unreachable!(),
                    }
                } else {
                    match command {
                        PlaybackCommand::PlayQueue {
                            tracks,
                            start_index,
                        } => {
                            if queue.replace(tracks, start_index) {
                                publish(&snapshot, |state| {
                                    if let Some(track) = queue.current() {
                                        state.current_id = Some(track.id);
                                        state.current_title = track.title.clone();
                                    }
                                    state.queue_index = queue.current_index().unwrap_or(0) + 1;
                                    state.queue_len = queue.len();
                                    state.state = PlaybackUiState::Unavailable;
                                    state.message = runtime_error.clone();
                                });
                            }
                        }
                        PlaybackCommand::Previous | PlaybackCommand::Next => {
                            publish(&snapshot, |state| state.message = runtime_error.clone());
                        }
                        PlaybackCommand::TogglePause | PlaybackCommand::Shutdown => {}
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }

        let Some(engine) = engine.as_mut() else {
            continue;
        };
        if queue.current().is_none() || lock_snapshot(&snapshot).state == PlaybackUiState::Error {
            continue;
        }

        match engine.state() {
            PlayerState::Ended if ended_generation != Some(generation) => {
                ended_generation = Some(generation);
                if queue.advance_next().is_some() {
                    generation = generation.wrapping_add(1);
                    ended_generation = None;
                    start_current(engine, &queue, &snapshot);
                } else {
                    publish_player_position(engine, &queue, &snapshot, "Queue finished.");
                    publish(&snapshot, |state| state.state = PlaybackUiState::Ended);
                }
            }
            PlayerState::Error => {
                let error = engine.error_message();
                let message = if error.is_empty() {
                    "VLC could not decode this track or open an audio output. Check the VLC plugins installation.".to_owned()
                } else {
                    format!("VLC reported an audio playback error: {error}")
                };
                publish_player_position(engine, &queue, &snapshot, &message);
                publish(&snapshot, |state| state.state = PlaybackUiState::Error);
            }
            state => publish_player_state(engine, state, &queue, &snapshot),
        }
    }

    drop(engine);
}

fn start_current(
    engine: &mut VlcEngine,
    queue: &PlaybackQueue,
    snapshot: &Mutex<PlaybackSnapshot>,
) {
    let Some(track) = queue.current().cloned() else {
        return;
    };

    publish(snapshot, |state| {
        state.current_id = Some(track.id);
        state.current_title = track.title.clone();
        state.queue_index = queue.current_index().unwrap_or(0) + 1;
        state.queue_len = queue.len();
        state.state = PlaybackUiState::Loading;
        state.position_ms = 0;
        state.duration_ms = 0;
        state.message = "Opening audio file…".to_owned();
    });

    if let Err(error) = engine.play(&track) {
        publish(snapshot, |state| {
            state.state = PlaybackUiState::Error;
            state.message = error.to_string();
        });
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
        state.current_title = queue
            .current()
            .map(|track| track.title.clone())
            .unwrap_or_default();
        state.queue_index = queue.current_index().map_or(0, |index| index + 1);
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
        current.current_title = queue
            .current()
            .map(|track| track.title.clone())
            .unwrap_or_default();
        current.queue_index = queue.current_index().map_or(0, |index| index + 1);
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
        if matches!(state, PlayerState::Playing | PlayerState::Paused) {
            current.message.clear();
        }
    });
}
