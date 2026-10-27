#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() {
    #[cfg(target_os = "macos")]
    rust_vlc::platform::configure_runtime_environment();

    let (database, _) = rust_vlc::storage::application_directories()
        .unwrap_or_else(|error| report_startup_error(error));
    let library_lock = rust_vlc::storage::LibraryLock::try_acquire(&database)
        .unwrap_or_else(|error| report_startup_error(error));

    let result = rust_vlc::app::run();
    drop(library_lock);
    if let Err(error) = result {
        report_startup_error(error);
    }
}

fn report_startup_error(error: impl std::fmt::Display) -> ! {
    let error = error.to_string();
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
