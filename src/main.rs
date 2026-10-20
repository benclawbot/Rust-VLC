fn main() {
    if let Err(error) = rust_vlc::app::run() {
        eprintln!("VLC Première could not start: {error}");
        std::process::exit(1);
    }
}
