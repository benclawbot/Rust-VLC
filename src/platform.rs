use std::env;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeCandidate {
    pub library_path: PathBuf,
    pub plugin_path: PathBuf,
}

pub fn runtime_candidates() -> Vec<RuntimeCandidate> {
    let mut candidates = Vec::new();

    if let Some(root) = env::var_os("VLC_DIR").map(PathBuf::from) {
        add_root_candidates(&mut candidates, &root);
    }

    if let Ok(executable) = env::current_exe() {
        if let Some(directory) = executable.parent() {
            #[cfg(target_os = "macos")]
            if let Some(root) = bundled_macos_vlc_root(directory) {
                add_root_candidates(&mut candidates, &root);
            }

            add_root_candidates(&mut candidates, &directory.join("VLC"));
            add_root_candidates(&mut candidates, &directory.join("vlc"));
            add_root_candidates(&mut candidates, directory);
        }
    }

    #[cfg(target_os = "windows")]
    {
        for variable in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(program_files) = env::var_os(variable) {
                add_root_candidates(
                    &mut candidates,
                    &PathBuf::from(program_files).join("VideoLAN").join("VLC"),
                );
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        add_root_candidates(
            &mut candidates,
            Path::new("/Applications/VLC.app/Contents/MacOS"),
        );
    }

    #[cfg(target_os = "linux")]
    {
        for root in [
            "/usr/lib",
            "/usr/lib64",
            "/usr/local/lib",
            "/usr/lib/x86_64-linux-gnu",
            "/usr/lib/aarch64-linux-gnu",
        ] {
            add_root_candidates(&mut candidates, Path::new(root));
        }
    }

    candidates
}

#[cfg(target_os = "macos")]
pub fn configure_runtime_environment() {
    // VLC reads these process paths during initialization; set them before the UI starts threads.
    if let Some(candidate) = first_complete_runtime_candidate(&runtime_candidates()) {
        let (plugin_path, data_path) = runtime_environment_paths(&candidate);
        env::set_var("VLC_PLUGIN_PATH", plugin_path);
        if let Some(data_path) = data_path.filter(|path| path.is_dir()) {
            env::set_var("VLC_DATA_PATH", data_path);
        }
    }
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn first_complete_runtime_candidate(
    candidates: &[RuntimeCandidate],
) -> Option<RuntimeCandidate> {
    candidates
        .iter()
        .find(|candidate| candidate.library_path.is_file() && candidate.plugin_path.is_dir())
        .cloned()
}

#[cfg(any(target_os = "macos", test))]
fn bundled_macos_vlc_root(executable_directory: &Path) -> Option<PathBuf> {
    executable_directory
        .parent()
        .map(|contents| contents.join("Frameworks/VLC.app/Contents/MacOS"))
}

#[cfg(any(target_os = "macos", test))]
fn runtime_environment_paths(candidate: &RuntimeCandidate) -> (PathBuf, Option<PathBuf>) {
    (
        candidate.plugin_path.clone(),
        candidate
            .plugin_path
            .parent()
            .map(|root| root.join("share")),
    )
}

fn add_root_candidates(output: &mut Vec<RuntimeCandidate>, root: &Path) {
    if !root.is_absolute() {
        return;
    }

    #[cfg(target_os = "windows")]
    let libraries = [root.join("libvlc.dll")];
    #[cfg(target_os = "macos")]
    let libraries = [root.join("libvlc.dylib"), root.join("lib/libvlc.dylib")];
    #[cfg(target_os = "linux")]
    let libraries = [root.join("libvlc.so.5"), root.join("libvlc.so")];
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    let libraries: [PathBuf; 0] = [];

    for library_path in libraries {
        let plugin_path = if library_path.parent() == Some(root) {
            #[cfg(target_os = "linux")]
            {
                root.join("vlc/plugins")
            }
            #[cfg(not(target_os = "linux"))]
            {
                root.join("plugins")
            }
        } else {
            root.join("plugins")
        };
        let candidate = RuntimeCandidate {
            library_path,
            plugin_path,
        };
        if !output.contains(&candidate) {
            output.push(candidate);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        bundled_macos_vlc_root, first_complete_runtime_candidate, runtime_candidates,
        runtime_environment_paths, RuntimeCandidate,
    };
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    #[test]
    fn discovered_runtime_candidates_never_use_relative_paths() {
        assert!(runtime_candidates()
            .iter()
            .all(|candidate| candidate.library_path.is_absolute()
                && candidate.plugin_path.is_absolute()));
    }

    #[test]
    fn bundled_macos_vlc_root_uses_frameworks_app_contents_macos() {
        let executable_directory = Path::new("/Applications/Rust-VLC.app/Contents/MacOS");
        let expected = PathBuf::from("/Applications/Rust-VLC.app/Contents")
            .join("Frameworks/VLC.app/Contents/MacOS");

        assert_eq!(bundled_macos_vlc_root(executable_directory), Some(expected));
    }

    #[test]
    fn runtime_environment_uses_the_selected_vlc_plugin_and_data_directories() {
        let candidate = super::RuntimeCandidate {
            library_path: PathBuf::from(
                "/Applications/Rust-VLC.app/Contents/Frameworks/VLC.app/Contents/MacOS/lib/libvlc.dylib",
            ),
            plugin_path: PathBuf::from(
                "/Applications/Rust-VLC.app/Contents/Frameworks/VLC.app/Contents/MacOS/plugins",
            ),
        };

        assert_eq!(
            runtime_environment_paths(&candidate),
            (
                candidate.plugin_path.clone(),
                Some(PathBuf::from(
                    "/Applications/Rust-VLC.app/Contents/Frameworks/VLC.app/Contents/MacOS/share"
                ))
            )
        );
    }

    #[test]
    fn first_complete_runtime_skips_missing_plugins_and_keeps_candidate_order() {
        let root = std::env::temp_dir().join(format!(
            "rust-vlc-runtime-candidate-{}-{}",
            std::process::id(),
            fastrand::u64(..)
        ));
        let incomplete = root.join("incomplete");
        let first = root.join("first");
        let second = root.join("second");
        fs::create_dir_all(&incomplete).unwrap();
        fs::write(incomplete.join("libvlc.dylib"), []).unwrap();
        for directory in [&first, &second] {
            fs::create_dir_all(directory.join("plugins")).unwrap();
            fs::write(directory.join("libvlc.dylib"), []).unwrap();
        }

        let candidates = [
            RuntimeCandidate {
                library_path: incomplete.join("libvlc.dylib"),
                plugin_path: incomplete.join("plugins"),
            },
            RuntimeCandidate {
                library_path: first.join("libvlc.dylib"),
                plugin_path: first.join("plugins"),
            },
            RuntimeCandidate {
                library_path: second.join("libvlc.dylib"),
                plugin_path: second.join("plugins"),
            },
        ];

        assert_eq!(
            first_complete_runtime_candidate(&candidates),
            Some(candidates[1].clone())
        );
        fs::remove_dir_all(root).unwrap();
    }
}
