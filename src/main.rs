#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() {
    #[cfg(target_os = "macos")]
    rust_vlc::platform::configure_runtime_environment();

    if let Err(error) = rust_vlc::app::run() {
        eprintln!("VLC Première could not start: {error}");
        rfd::MessageDialog::new()
            .set_title("Rust-VLC")
            .set_description(format!(
                "The application could not complete the operation: {error}"
            ))
            .set_level(rfd::MessageLevel::Error)
            .show();
        std::process::exit(1);
    }
}
