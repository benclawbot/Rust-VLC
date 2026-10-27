use crate::metadata::{self, ArtworkRequest, LocalMetadata};
use crate::scan::{self, ScanOutcome};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const ARTWORK_QUEUE_CAPACITY: usize = 8;

pub enum LibraryJob {
    Scan(PathBuf),
    ReadMetadata(Vec<(i32, PathBuf)>),
    Artwork(i32, u64, ArtworkRequest),
    SearchTmdbCandidates(i32, u64, ArtworkRequest),
    SelectTmdbCandidate(i32, u64, metadata::ArtworkKind, u64),
    LookupTmdbGenres(i32, u64, ArtworkRequest),
    SearchSubtitles {
        request_id: u64,
        query: String,
        languages: Vec<String>,
    },
    DownloadSubtitle {
        request_id: u64,
        file_id: i32,
    },
    Shutdown,
}

pub enum LibraryEvent {
    Scanned(PathBuf, ScanOutcome),
    Metadata(i32, PathBuf, Result<LocalMetadata, String>),
    Artwork(i32, u64, Result<Option<PathBuf>, String>),
    TmdbCandidates(i32, u64, Result<Vec<metadata::TmdbCandidate>, String>),
    TmdbCandidateSelected(i32, u64, Result<metadata::TmdbArtwork, String>),
    TmdbGenres(i32, u64, Result<Option<String>, String>),
    AutomaticArtwork(i32, ArtworkRequest, Result<Option<PathBuf>, String>),
    SubtitlesFound(
        u64,
        Result<Vec<crate::subtitles::SubtitleSearchResult>, String>,
    ),
    SubtitleDownloaded(u64, Result<PathBuf, String>),
}

enum ArtworkJob {
    Manual(i32, u64, ArtworkRequest),
    Automatic(i32, ArtworkRequest),
    SearchTmdbCandidates(i32, u64, ArtworkRequest),
    SelectTmdbCandidate(i32, u64, metadata::ArtworkKind, u64),
    LookupTmdbGenres(i32, u64, ArtworkRequest),
    SearchSubtitles {
        request_id: u64,
        query: String,
        languages: Vec<String>,
    },
    DownloadSubtitle {
        request_id: u64,
        file_id: i32,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueueSubmitError {
    Full,
    Stopped,
}

impl QueueSubmitError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Full => {
                "The library worker is busy. Try again after the current import or lookup finishes."
            }
            Self::Stopped => "The library worker stopped. Restart the application.",
        }
    }
}

pub struct LibraryWorker {
    sender: mpsc::SyncSender<LibraryJob>,
    artwork_sender: mpsc::SyncSender<ArtworkJob>,
    receiver: Option<mpsc::Receiver<LibraryEvent>>,
    stopping: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    automatic_join: Option<JoinHandle<()>>,
}

impl LibraryWorker {
    pub fn spawn(cache: PathBuf) -> Result<Self, String> {
        let (sender, jobs) = mpsc::sync_channel(4);
        let (events, receiver) = mpsc::sync_channel(32);
        let (artwork_sender, artwork_jobs) = mpsc::sync_channel(ARTWORK_QUEUE_CAPACITY);
        let stopping = Arc::new(AtomicBool::new(false));
        let automatic_stopping = Arc::clone(&stopping);
        let automatic_events = events.clone();
        let automatic_cache = cache.clone();
        let artwork_join = thread::Builder::new()
            .name("artwork-lookup".into())
            .spawn(move || {
                while !automatic_stopping.load(Ordering::Relaxed) {
                    let job = match artwork_jobs.recv_timeout(Duration::from_millis(50)) {
                        Ok(job) => job,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    let event = match job {
                        ArtworkJob::Manual(id, generation, request) => {
                            let result = if request.kind == metadata::ArtworkKind::Music {
                                metadata::lookup_artwork_with_cancel(
                                    &request,
                                    &automatic_cache,
                                    None,
                                    &automatic_stopping,
                                )
                            } else {
                                crate::credentials::tmdb_token().and_then(|token| {
                                    metadata::lookup_artwork_with_cancel(
                                        &request,
                                        &automatic_cache,
                                        token.as_deref(),
                                        &automatic_stopping,
                                    )
                                })
                            };
                            LibraryEvent::Artwork(id, generation, result)
                        }
                        ArtworkJob::Automatic(id, request) => {
                            let result = metadata::lookup_artwork_with_cancel(
                                &request,
                                &automatic_cache,
                                None,
                                &automatic_stopping,
                            );
                            LibraryEvent::AutomaticArtwork(id, request, result)
                        }
                        ArtworkJob::SearchTmdbCandidates(id, generation, request) => {
                            let result = crate::credentials::tmdb_token().and_then(|token| {
                                metadata::search_tmdb_candidates_with_cancel(
                                    &request,
                                    &automatic_cache,
                                    token.as_deref(),
                                    &automatic_stopping,
                                )
                            });
                            LibraryEvent::TmdbCandidates(id, generation, result)
                        }
                        ArtworkJob::SelectTmdbCandidate(id, generation, kind, tmdb_id) => {
                            let result = crate::credentials::tmdb_token().and_then(|token| {
                                metadata::download_tmdb_candidate_with_cancel(
                                    kind,
                                    tmdb_id,
                                    &automatic_cache,
                                    token.as_deref(),
                                    &automatic_stopping,
                                )
                            });
                            LibraryEvent::TmdbCandidateSelected(id, generation, result)
                        }
                        ArtworkJob::LookupTmdbGenres(id, generation, request) => {
                            let result = crate::credentials::tmdb_token().and_then(|token| {
                                metadata::lookup_tmdb_genres_with_cancel(
                                    &request,
                                    &automatic_cache,
                                    token.as_deref(),
                                    &automatic_stopping,
                                )
                            });
                            LibraryEvent::TmdbGenres(id, generation, result)
                        }
                        ArtworkJob::SearchSubtitles {
                            request_id,
                            query,
                            languages,
                        } => LibraryEvent::SubtitlesFound(
                            request_id,
                            crate::subtitles::search_subtitles_with_cancel(
                                &query,
                                &languages,
                                &automatic_stopping,
                            ),
                        ),
                        ArtworkJob::DownloadSubtitle {
                            request_id,
                            file_id,
                        } => LibraryEvent::SubtitleDownloaded(
                            request_id,
                            crate::subtitles::download_subtitle_with_cancel(
                                file_id,
                                &automatic_cache,
                                &automatic_stopping,
                            ),
                        ),
                    };
                    if automatic_stopping.load(Ordering::Relaxed)
                        || automatic_events.send(event).is_err()
                    {
                        break;
                    }
                }
            })
            .map_err(|error| error.to_string())?;
        let worker_stopping = Arc::clone(&stopping);
        let join_result = thread::Builder::new()
            .name("library-metadata".into())
            .spawn(move || {
                while !worker_stopping.load(Ordering::Relaxed) {
                    let Ok(job) = jobs.recv() else {
                        break;
                    };
                    let event = match job {
                        LibraryJob::Scan(root) => {
                            let outcome = scan::scan_directory_with_cancel(&root, &worker_stopping);
                            LibraryEvent::Scanned(root, outcome)
                        }
                        LibraryJob::ReadMetadata(paths) => {
                            for (id, path) in paths {
                                if worker_stopping.load(Ordering::Relaxed) {
                                    break;
                                }
                                let result = metadata::read_local(&path, &cache);
                                if events
                                    .send(LibraryEvent::Metadata(id, path, result))
                                    .is_err()
                                {
                                    return;
                                }
                            }
                            continue;
                        }
                        LibraryJob::Shutdown => break,
                        LibraryJob::Artwork(_, _, _) => {
                            unreachable!("artwork is routed separately")
                        }
                        LibraryJob::SearchTmdbCandidates(_, _, _)
                        | LibraryJob::SelectTmdbCandidate(_, _, _, _)
                        | LibraryJob::LookupTmdbGenres(_, _, _) => {
                            unreachable!("TMDB candidate jobs are routed separately")
                        }
                        LibraryJob::SearchSubtitles { .. }
                        | LibraryJob::DownloadSubtitle { .. } => {
                            unreachable!("subtitle jobs are routed separately")
                        }
                    };
                    if events.send(event).is_err() {
                        break;
                    }
                }
            });
        let join = match join_result {
            Ok(join) => join,
            Err(error) => {
                drop(artwork_sender);
                let _ = artwork_join.join();
                return Err(error.to_string());
            }
        };
        Ok(Self {
            sender,
            artwork_sender,
            receiver: Some(receiver),
            stopping,
            join: Some(join),
            automatic_join: Some(artwork_join),
        })
    }

    pub fn submit(&self, job: LibraryJob) -> Result<(), String> {
        self.try_submit(job)
            .map_err(|error| error.message().to_owned())
    }

    pub fn try_submit(&self, job: LibraryJob) -> Result<(), QueueSubmitError> {
        match job {
            LibraryJob::Artwork(id, generation, request) => self
                .artwork_sender
                .try_send(ArtworkJob::Manual(id, generation, request))
                .map_err(submission_error),
            LibraryJob::SearchTmdbCandidates(id, generation, request) => self
                .artwork_sender
                .try_send(ArtworkJob::SearchTmdbCandidates(id, generation, request))
                .map_err(submission_error),
            LibraryJob::SelectTmdbCandidate(id, generation, kind, tmdb_id) => self
                .artwork_sender
                .try_send(ArtworkJob::SelectTmdbCandidate(
                    id, generation, kind, tmdb_id,
                ))
                .map_err(submission_error),
            LibraryJob::LookupTmdbGenres(id, generation, request) => self
                .artwork_sender
                .try_send(ArtworkJob::LookupTmdbGenres(id, generation, request))
                .map_err(submission_error),
            LibraryJob::SearchSubtitles {
                request_id,
                query,
                languages,
            } => self
                .artwork_sender
                .try_send(ArtworkJob::SearchSubtitles {
                    request_id,
                    query,
                    languages,
                })
                .map_err(submission_error),
            LibraryJob::DownloadSubtitle {
                request_id,
                file_id,
            } => self
                .artwork_sender
                .try_send(ArtworkJob::DownloadSubtitle {
                    request_id,
                    file_id,
                })
                .map_err(submission_error),
            job => self.sender.try_send(job).map_err(submission_error),
        }
    }

    pub fn try_submit_automatic_artwork(
        &self,
        id: i32,
        request: ArtworkRequest,
    ) -> Result<(), QueueSubmitError> {
        self.artwork_sender
            .try_send(ArtworkJob::Automatic(id, request))
            .map_err(submission_error)
    }

    pub fn drain_events(&self) -> Vec<LibraryEvent> {
        self.receiver
            .as_ref()
            .map(|receiver| receiver.try_iter().take(32).collect())
            .unwrap_or_default()
    }

    pub fn shutdown(&mut self) {
        self.stopping.store(true, Ordering::Relaxed);
        // A blocked publisher must be released before joining the worker.
        self.receiver.take();
        let _ = self.sender.try_send(LibraryJob::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        if let Some(join) = self.automatic_join.take() {
            let _ = join.join();
        }
    }
}

fn submission_error<T>(error: mpsc::TrySendError<T>) -> QueueSubmitError {
    match error {
        mpsc::TrySendError::Full(_) => QueueSubmitError::Full,
        mpsc::TrySendError::Disconnected(_) => QueueSubmitError::Stopped,
    }
}

impl Drop for LibraryWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, Instant};

    #[test]
    fn artwork_results_echo_the_intent_generation_on_validation_errors() {
        let worker = LibraryWorker::spawn(std::env::temp_dir()).unwrap();
        worker
            .submit(LibraryJob::Artwork(
                17,
                42,
                ArtworkRequest {
                    title: String::new(),
                    artist: None,
                    album: None,
                    kind: crate::metadata::ArtworkKind::Movie,
                },
            ))
            .unwrap();
        let receiver = worker.receiver.as_ref().unwrap();
        let event = receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("worker returns the artwork validation result");
        match event {
            LibraryEvent::Artwork(id, generation, Err(_)) => {
                assert_eq!(id, 17);
                assert_eq!(generation, 42);
            }
            _ => panic!("expected an artwork validation error"),
        }
    }

    #[test]
    fn manual_music_artwork_does_not_depend_on_the_tmdb_credential_store() {
        let worker = LibraryWorker::spawn(std::env::temp_dir()).unwrap();
        worker
            .submit(LibraryJob::Artwork(
                17,
                42,
                ArtworkRequest {
                    title: String::new(),
                    artist: None,
                    album: None,
                    kind: crate::metadata::ArtworkKind::Music,
                },
            ))
            .unwrap();
        let event = worker
            .receiver
            .as_ref()
            .unwrap()
            .recv_timeout(Duration::from_secs(2))
            .expect("music lookup returns validation before any network access");
        assert!(matches!(
            event,
            LibraryEvent::Artwork(17, 42, Err(message))
                if message == "A title of at most 512 bytes is required."
        ));
    }

    #[test]
    fn full_library_and_artwork_queues_return_without_blocking() {
        let (sender, _jobs) = mpsc::sync_channel(1);
        let (artwork_sender, _artwork_jobs) = mpsc::sync_channel(1);
        let queue_library_job = sender.clone();
        let queue_artwork_job = artwork_sender.clone();
        let (_events, receiver) = mpsc::sync_channel(1);
        let worker = LibraryWorker {
            sender,
            artwork_sender,
            receiver: Some(receiver),
            stopping: Arc::new(AtomicBool::new(false)),
            join: None,
            automatic_join: None,
        };
        queue_library_job
            .try_send(LibraryJob::Scan(PathBuf::from("queued")))
            .unwrap();
        queue_artwork_job
            .try_send(ArtworkJob::Automatic(
                1,
                ArtworkRequest {
                    title: "Queued".into(),
                    artist: None,
                    album: None,
                    kind: crate::metadata::ArtworkKind::Music,
                },
            ))
            .unwrap();

        let started = Instant::now();
        assert_eq!(
            worker.try_submit(LibraryJob::Scan(PathBuf::from("overflow"))),
            Err(QueueSubmitError::Full)
        );
        assert_eq!(
            worker.try_submit_automatic_artwork(
                2,
                ArtworkRequest {
                    title: "Overflow".into(),
                    artist: None,
                    album: None,
                    kind: crate::metadata::ArtworkKind::Music,
                }
            ),
            Err(QueueSubmitError::Full)
        );
        assert_eq!(
            worker.try_submit(LibraryJob::SearchSubtitles {
                request_id: 8,
                query: "title".into(),
                languages: vec!["en".into()],
            }),
            Err(QueueSubmitError::Full)
        );
        assert_eq!(
            worker.try_submit(LibraryJob::DownloadSubtitle {
                request_id: 9,
                file_id: 12,
            }),
            Err(QueueSubmitError::Full)
        );
        assert!(started.elapsed() < Duration::from_millis(50));
    }

    #[test]
    fn shutdown_releases_library_worker_when_ui_stops_draining_events() {
        let mut worker = LibraryWorker::spawn(std::env::temp_dir()).unwrap();
        worker
            .submit(LibraryJob::ReadMetadata(
                (0..100)
                    .map(|id| (id, PathBuf::from("not-a-real-file.mp3")))
                    .collect(),
            ))
            .unwrap();
        std::thread::sleep(Duration::from_millis(50));
        let started = Instant::now();
        worker.shutdown();
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn shutdown_cancels_an_active_directory_scan_promptly() {
        let root = std::env::temp_dir().join(format!(
            "rust-vlc-worker-scan-{}-{}",
            std::process::id(),
            fastrand::u64(..)
        ));
        fs::create_dir(&root).expect("create scan fixture");
        for index in 0..10_000 {
            fs::write(root.join(format!("{index:05}.mp3")), []).expect("create scan entry fixture");
        }

        let mut worker = LibraryWorker::spawn(std::env::temp_dir()).unwrap();
        worker.submit(LibraryJob::Scan(root.clone())).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        let started = Instant::now();
        worker.shutdown();

        assert!(started.elapsed() < Duration::from_secs(2));
        fs::remove_dir_all(root).expect("remove scan fixture");
    }

    #[test]
    fn scan_event_echoes_the_root_with_its_outcome() {
        let root = std::env::temp_dir().join(format!(
            "rust-vlc-worker-root-{}-{}",
            std::process::id(),
            fastrand::u64(..)
        ));
        fs::create_dir(&root).expect("create scan fixture");
        fs::write(root.join("track.mp3"), []).expect("create media fixture");
        let worker = LibraryWorker::spawn(std::env::temp_dir()).unwrap();
        worker.submit(LibraryJob::Scan(root.clone())).unwrap();

        let event = worker
            .receiver
            .as_ref()
            .unwrap()
            .recv_timeout(Duration::from_secs(2))
            .expect("worker returns the scan result");
        match event {
            LibraryEvent::Scanned(scanned_root, outcome) => {
                assert_eq!(scanned_root, root);
                assert!(outcome.complete);
                assert_eq!(outcome.paths, [root.join("track.mp3")]);
            }
            _ => panic!("expected a scan result"),
        }
        fs::remove_dir_all(root).expect("remove scan fixture");
    }
}
