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
    use super::runtime_candidates;

    #[test]
    fn discovered_runtime_candidates_never_use_relative_paths() {
        assert!(runtime_candidates()
            .iter()
            .all(|candidate| candidate.library_path.is_absolute()
                && candidate.plugin_path.is_absolute()));
    }
}
