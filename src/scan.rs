use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

const MAX_SCAN_ENTRIES: usize = 100_000;
const MAX_SCAN_DEPTH: usize = 64;
const MAX_WARNINGS: usize = 128;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScanOutcome {
    pub paths: Vec<PathBuf>,
    pub warnings: Vec<String>,
    pub complete: bool,
}

/// Enumerate supported local audio and video files without following symlinks.
/// Results are sorted by their native paths. A bounded or failed traversal is incomplete.
pub fn scan_directory(root: &Path) -> ScanOutcome {
    static NEVER_CANCEL: AtomicBool = AtomicBool::new(false);
    scan_directory_with_cancel(root, &NEVER_CANCEL)
}

/// Enumerate supported files with cooperative cancellation between directory and entry I/O.
/// A filesystem call already in progress cannot be interrupted; cancellation is observed as
/// soon as that call returns.
pub fn scan_directory_with_cancel(root: &Path, cancel: &AtomicBool) -> ScanOutcome {
    let mut outcome = ScanOutcome {
        complete: true,
        ..ScanOutcome::default()
    };
    let mut omitted_warnings = 0usize;

    if mark_cancelled(cancel, &mut outcome, &mut omitted_warnings) {
        finish_warnings(&mut outcome.warnings, omitted_warnings);
        return outcome;
    }

    let root_metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) => {
            outcome.complete = false;
            push_warning(
                &mut outcome.warnings,
                &mut omitted_warnings,
                format!("Unable to inspect the scan root: {error}"),
            );
            finish_warnings(&mut outcome.warnings, omitted_warnings);
            return outcome;
        }
    };
    if root_metadata.file_type().is_symlink() {
        outcome.complete = false;
        push_warning(
            &mut outcome.warnings,
            &mut omitted_warnings,
            "The scan root is a symbolic link and was not followed.".to_owned(),
        );
        finish_warnings(&mut outcome.warnings, omitted_warnings);
        return outcome;
    }
    if !root_metadata.is_dir() {
        outcome.complete = false;
        push_warning(
            &mut outcome.warnings,
            &mut omitted_warnings,
            "The scan root is not a directory.".to_owned(),
        );
        finish_warnings(&mut outcome.warnings, omitted_warnings);
        return outcome;
    }

    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut entries_seen = 0usize;

    'walk: while let Some((directory, depth)) = stack.pop() {
        if mark_cancelled(cancel, &mut outcome, &mut omitted_warnings) {
            break 'walk;
        }
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                if mark_cancelled(cancel, &mut outcome, &mut omitted_warnings) {
                    break 'walk;
                }
                outcome.complete = false;
                push_warning(
                    &mut outcome.warnings,
                    &mut omitted_warnings,
                    format!("Unable to read a directory: {error}"),
                );
                continue;
            }
        };
        if mark_cancelled(cancel, &mut outcome, &mut omitted_warnings) {
            break 'walk;
        }

        let mut children = Vec::new();
        for entry in entries {
            if mark_cancelled(cancel, &mut outcome, &mut omitted_warnings) {
                break 'walk;
            }
            if entries_seen == MAX_SCAN_ENTRIES {
                outcome.complete = false;
                push_warning(
                    &mut outcome.warnings,
                    &mut omitted_warnings,
                    format!("Scan stopped at the {MAX_SCAN_ENTRIES}-entry limit."),
                );
                break 'walk;
            }
            entries_seen += 1;
            match entry {
                Ok(entry) => children.push(entry),
                Err(error) => {
                    outcome.complete = false;
                    push_warning(
                        &mut outcome.warnings,
                        &mut omitted_warnings,
                        format!("Unable to inspect a directory entry: {error}"),
                    );
                }
            }
        }
        children.sort_by_key(|entry| entry.file_name());

        for entry in children {
            if mark_cancelled(cancel, &mut outcome, &mut omitted_warnings) {
                break 'walk;
            }
            let entry_type = match entry.file_type() {
                Ok(entry_type) => entry_type,
                Err(error) => {
                    outcome.complete = false;
                    push_warning(
                        &mut outcome.warnings,
                        &mut omitted_warnings,
                        format!("Unable to inspect a directory entry: {error}"),
                    );
                    continue;
                }
            };

            if entry_type.is_symlink() {
                continue;
            }

            let path = entry.path();
            if entry_type.is_dir() {
                if depth == MAX_SCAN_DEPTH {
                    outcome.complete = false;
                    push_warning(
                        &mut outcome.warnings,
                        &mut omitted_warnings,
                        format!("Scan stopped at the {MAX_SCAN_DEPTH}-directory depth limit."),
                    );
                } else {
                    stack.push((path, depth + 1));
                }
            } else if entry_type.is_file() && is_supported_media_path(&path) {
                outcome.paths.push(path);
            }
        }
    }

    outcome.paths.sort();
    finish_warnings(&mut outcome.warnings, omitted_warnings);
    outcome
}

fn mark_cancelled(
    cancel: &AtomicBool,
    outcome: &mut ScanOutcome,
    omitted_warnings: &mut usize,
) -> bool {
    if cancel.load(Ordering::Relaxed) {
        outcome.complete = false;
        push_warning(
            &mut outcome.warnings,
            omitted_warnings,
            "Scan was canceled.".to_owned(),
        );
        true
    } else {
        false
    }
}

fn push_warning(warnings: &mut Vec<String>, omitted: &mut usize, warning: String) {
    if warnings.len() < MAX_WARNINGS {
        warnings.push(warning);
    } else {
        *omitted += 1;
    }
}

fn finish_warnings(warnings: &mut Vec<String>, omitted: usize) {
    if omitted > 0 {
        warnings.push(format!("{omitted} additional scan warning(s) omitted."));
    }
}

fn is_supported_media_path(path: &Path) -> bool {
    const AUDIO_EXTENSIONS: &[&str] = &[
        "aac", "aif", "aiff", "alac", "ape", "flac", "m4a", "m4b", "mid", "midi", "mp3", "mpc",
        "oga", "ogg", "opus", "wav", "wma", "wv",
    ];
    const VIDEO_EXTENSIONS: &[&str] = &[
        "3g2", "3gp", "asf", "avi", "divx", "dv", "flv", "m2ts", "m2v", "m4v", "mkv", "mov", "mp2",
        "mp4", "mpe", "mpeg", "mpg", "mts", "mxf", "ogv", "qt", "rm", "rmvb", "ts", "vob", "webm",
        "wmv",
    ];

    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            AUDIO_EXTENSIONS
                .iter()
                .chain(VIDEO_EXTENSIONS)
                .any(|allowed| extension.eq_ignore_ascii_case(allowed))
        })
}

#[cfg(test)]
mod tests {
    use super::{scan_directory, scan_directory_with_cancel};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_directory() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after the Unix epoch")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("rust-vlc-scan-{}-{nonce}", std::process::id()));
        fs::create_dir(&root).expect("create isolated scan fixture");
        root
    }

    #[test]
    fn scans_supported_media_recursively_and_sorts_native_paths() {
        let root = temporary_directory();
        let nested = root.join("disc 1");
        fs::create_dir(&nested).expect("create nested fixture directory");
        for path in [
            root.join("zeta.MP3"),
            root.join("movie.mkv"),
            root.join("notes.txt"),
            nested.join("été.flac"),
            nested.join("cover.jpg"),
        ] {
            fs::write(path, []).expect("create fixture file");
        }

        let mut expected = vec![
            root.join("movie.mkv"),
            root.join("zeta.MP3"),
            nested.join("été.flac"),
        ];
        expected.sort();
        let outcome = scan_directory(&root);

        assert!(outcome.complete, "{:?}", outcome.warnings);
        assert!(outcome.warnings.is_empty());
        assert_eq!(outcome.paths, expected);
        fs::remove_dir_all(root).expect("remove isolated scan fixture");
    }

    #[test]
    fn missing_root_is_reported_as_incomplete() {
        let root = temporary_directory();
        fs::remove_dir(&root).expect("remove isolated scan fixture");

        let outcome = scan_directory(&root);

        assert!(!outcome.complete);
        assert!(outcome.paths.is_empty());
        assert!(!outcome.warnings.is_empty());
    }

    #[test]
    fn pre_cancelled_scan_is_incomplete_with_one_cancellation_warning() {
        let root = temporary_directory();
        fs::write(root.join("track.mp3"), []).expect("create media fixture");
        let cancel = AtomicBool::new(true);

        let outcome = scan_directory_with_cancel(&root, &cancel);

        assert!(!outcome.complete);
        assert!(outcome.paths.is_empty());
        assert_eq!(
            outcome
                .warnings
                .iter()
                .filter(|warning| warning.as_str() == "Scan was canceled.")
                .count(),
            1
        );
        fs::remove_dir_all(root).expect("remove isolated scan fixture");
    }

    #[cfg(unix)]
    #[test]
    fn does_not_follow_directory_symlinks() {
        use std::os::unix::fs::symlink;

        let root = temporary_directory();
        let outside = temporary_directory();
        fs::write(outside.join("secret.mp3"), []).expect("create external fixture");
        symlink(&outside, root.join("linked")).expect("create directory symlink");
        fs::write(root.join("local.mp3"), []).expect("create local fixture");

        let outcome = scan_directory(&root);

        assert!(outcome.complete);
        assert_eq!(outcome.paths, [root.join("local.mp3")]);
        fs::remove_dir_all(root).expect("remove isolated scan fixture");
        fs::remove_dir_all(outside).expect("remove isolated scan fixture");
    }
}
