# Rust-VLC 0.1.2

This release adds persistent playback sessions and more desktop playback tools while keeping the selected Movies/Music layout.

- Restore the queue, duplicate occurrence, position, repeat mode and shuffled traversal without autoplaying.
- Watch imported folders at startup and periodically; reconcile changed or missing files while keeping history and source files intact.
- Import and export M3U8 playlists, including repeated entries and relative paths.
- Adjust the ten-band equalizer, use presets, save bookmarks, set A–B repeat and schedule a sleep timer.
- Save the decoded video frame as PNG and use picture-in-picture with the same playback session.
- Open video or audio network streams, retain local history, search/download subtitles through configured OpenSubtitles credentials, and configure a Chromecast target manually.
- Integrate desktop media controls where the operating system provides them.

The update also repairs album ordering, synchronization-field drafts, returning to the existing movie session, Windows watched-folder identity, URL and playlist persistence edge cases, and native VLC error handling. Dependency updates address the RustSec advisories identified during validation.

Parental/PIN locking and paired LAN remote control are excluded as requested. Casting needs a compatible receiver; subtitle downloads need provider credentials. These device/provider flows and native macOS/Arch playback must be qualified separately from Rust tests.

## Downloads

Download the four platform installers and their SHA-256 checksums from the [v0.1.2 release](https://github.com/benclawbot/Rust-VLC/releases/tag/v0.1.2). The published asset digests match the independently downloaded CI artifacts.

The release workflow builds Windows x86_64, Arch Linux x86_64, macOS Apple Silicon and macOS Intel installers, audits the locked dependencies and checks every installer checksum before publishing. Validation results and remaining native-device checks are recorded in the [validation report](https://github.com/benclawbot/Rust-VLC/blob/main/docs/VALIDATION.md).
