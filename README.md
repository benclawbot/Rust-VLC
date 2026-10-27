# Rust-VLC

A local movie and music player built with Rust, Slint and libVLC 3, inspired by [VLC Première](https://github.com/benclawbot/VLC-Premiere). Movies and Music share one player, so browsing another collection keeps playback running.

The current release is **0.1.2**. See the [release notes](docs/RELEASE_0.1.2.md) for the latest changes.

## Get the application

Download an installer from the [Rust-VLC v0.1.2 release](https://github.com/benclawbot/Rust-VLC/releases/tag/v0.1.2). Each installer has a matching `.sha256` checksum beside it.

| Platform | Installer | Install |
| --- | --- | --- |
| Arch Linux x86_64 | `rust-vlc-0.1.2-1-x86_64.pkg.tar.zst` | Run `sudo pacman -U rust-vlc-0.1.2-1-x86_64.pkg.tar.zst`; pacman installs libVLC 3 and the full plugin set as dependencies |
| Windows x86_64 | `rust-vlc-windows-x86_64-setup.exe` | Run Setup; it includes VLC 3 and creates a desktop shortcut by default |
| macOS Apple Silicon | `rust-vlc-macos-arm64.pkg` | Open the package; it installs Rust-VLC and its VLC runtime in Applications |
| macOS Intel | `rust-vlc-macos-x86_64.pkg` | Open the package; it installs Rust-VLC and its VLC runtime in Applications |

The repository is private, so release downloads require repository access. Windows and macOS installers bundle the matching VLC 3.0.24 runtime and plugins. The Arch package depends directly on libVLC and the full VLC plugin set, so pacman installs them with Rust-VLC without requiring the VLC desktop application. Linux adds an app-launcher entry and icon; Windows creates a desktop shortcut by default. The installers are unsigned, and macOS packages are not notarized, so the operating system may warn during install or first launch. The Windows executable includes the MSVC runtime and does not need a separate Visual C++ Redistributable. See [runtime setup](packaging/RUNTIME.md) for details.

Development executables are available from the [Actions workflow](https://github.com/benclawbot/Rust-VLC/actions/workflows/dev-artifacts.yml); those artifacts are for testing and still require a separately installed VLC 3 runtime.

## Use your library

Add local files or import a folder. Imported folders become watched folders: the app scans them at startup, periodically and through **Options → Library folders → Rescan**. Removing a watched folder stops watching it and keeps imported media, playlists and history. Complete scans report missing media; incomplete or disconnected scans retain existing entries. Source media is read-only. Library data stays in application data directories, outside your media folders.

- **Movies:** browse posters and series/seasons, search, filter watch progress, and resume or start over. Double-click a movie to play fullscreen; Escape returns to the library while playback continues. **Return to video** or **F** restores the existing movie without loading it again. The three central transport icons hide during inactive playback.
- **Music:** browse Albums, Artists, Tracks and Genres with real tags and artwork. Albums start in disc/track order. Click a song to start its displayed collection at that song. Leading checkboxes select tracks without starting playback; Select all and Play selected use the displayed rows in order.
- **Playlists:** create a named playlist, choose an existing destination for checked tracks, open its contents, reorder/remove entries and shuffle its stored order. An explicit duplicate option preserves repeated songs when wanted. Import and export M3U8 playlists with relative paths and repeated entries. Exports contain media references rather than copies of the files. Playlist data survives restart.
- **Playback:** previous/next, play/pause, stop, seek, volume, speed, repeat and playback shuffle. The mini-player uses a centered icon transport with accessible action names. Space toggles play/pause, Left/Right seeks ten seconds, and Ctrl+Left/Right changes media in both the video and music player. Movies play fullscreen with centered previous/play-pause/next icons. Player tools show available audio/subtitle tracks, subtitle files, synchronization delays, jump-to-time, available chapters and fit/fill video display.

Missing files and unavailable VLC plugins remain visible as errors rather than being silently removed from the library. A future database schema is preserved; recognized upgrades first create a recovery backup. Library databases retain native paths and are specific to the operating system that created them.

The queue, current occurrence and position, repeat mode and shuffled traversal survive restart. Restored queues wait for an explicit Play action; opening the app never autoplays.

## Playback tools and network media

**Tools** includes equalizer presets and ten-band adjustment, named bookmarks, A–B repeat and a cancellable sleep timer. Equalizer settings are retained between sessions. Synchronization fields keep draft edits until Apply. Save the actual displayed video frame as a PNG, or open picture-in-picture using the toolbar or **P**. Picture-in-picture shares the current playback session and stays above other windows where the operating system permits. OS media controls expose playback and metadata when the desktop media service is available.

Open an HTTP, HTTPS, RTSP or RTMP address from Network tools and choose video or audio-only playback. Stream history is stored locally. Embedded usernames/passwords are rejected; signed query strings remain in the local database so saved streams can be replayed, while labels and diagnostic messages redact them. Stream availability and seeking depend on the server.

Casting accepts a manually entered Chromecast IP address or hostname, plus a local-output option. Configuring a target does not confirm that a receiver is reachable; receiver and codec support require a compatible device.

Configure an **OpenSubtitles API key** in Options to search by title and language. Downloading also requires a bearer token and is subject to the account's limits. Downloaded files are checked and cached locally; they are attached only if the media selected when the request began is still current. Local subtitle files work without an online account.

## Artwork and privacy

Local tags, embedded pictures and nearby `cover`, `folder` or `poster` images are read first. Online artwork requests contain music or movie metadata, never local file paths.

- **Movies and TV:** choose **Options → Artwork** to save a TMDB Read Access Token. It is stored in the operating system credential store (Windows Credential Manager, macOS Keychain, or Linux Secret Service) and never in the library database. `TMDB_READ_TOKEN` remains available for existing environment-based setups.
- **Music:** after local tags are read, the background artwork worker searches public YouTube results with the track title and artist. It applies a thumbnail only when the match is strong, then uses the existing MusicBrainz/Cover Art Archive lookup as a fallback. The fallback can also send the album tag. No per-song YouTube URL is required.

OpenSubtitles credentials use the same operating system credential store as the TMDB token, never the library database. Stored secrets are not prefilled or logged. Subtitle searches send title/language metadata rather than native media paths.

YouTube matching reads bounded public search-page data without running JavaScript or using sign-in cookies. YouTube can change that page format, which may temporarily stop automatic matches until the parser is updated. Missing or ambiguous matches keep the local fallback artwork.

Downloaded artwork is validated, resized and cached. Cached covers remain usable offline; ambiguous matches can produce no artwork. Provider availability and credentials affect online lookup, while local playback continues independently.

This product uses the TMDB API but is not endorsed or certified by TMDB. See [TMDB attribution](https://developer.themoviedb.org/docs/faq).

## Third-party components

Windows and macOS releases include the unmodified VLC 3.0.24 runtime; Arch installs libVLC and its full plugin set from official package repositories. VLC components carry multiple open-source licenses: libVLC is LGPL 2.1-or-later, while individual modules and bundled libraries have their own terms. The installers include VLC notices and link to the corresponding source release in [third-party notices](packaging/VLC-THIRD-PARTY.md). Inter's OFL notice is included as well. Rust-VLC's application license has not yet been selected.

## Interface

The selected **Archive style** uses Première's Inter typography, dark brown surfaces, white text and orange actions, with a prominent Movies / Music switch at the top left. Focus and selection use the same orange palette. “Archive” is the design style; the movie page is titled **Movies**.

The following screenshots were captured from the native 0.1.2 Rust/Slint Movies and Music screens using the selected [Archive design reference](design/proposals/archive.html). The screenshots use an isolated sample library; sample media and metadata are not bundled with the application.

![Native Movies screen using the isolated sample library](docs/screenshots/movies.jpg)

![Native Music screen using the isolated sample library](docs/screenshots/music.jpg)

## Build from source

Install the Rust toolchain pinned in `rust-toolchain.toml`, a C/C++ build toolchain with CMake, platform build dependencies and a compatible VLC 3 installation. Windows MSVC builds also use the Windows SDK resource compiler for application icon/version information. Start the application with:

```sh
cargo run --locked
```

If VLC is outside a standard installation directory, set `VLC_DIR` to its absolute directory. VLC, its plugins and Rust-VLC must have matching CPU architectures. Arch uses its system VLC installation; Linux multiarch library directories and `/Applications/VLC.app` are also searched.

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
cargo build --release --locked
cargo audit --file Cargo.lock
```

The [build workflow](.github/workflows/dev-artifacts.yml) creates development archives on native runners. The [release workflow](.github/workflows/release-installers.yml) builds the Arch package, Windows Setup executable, and separate Apple Silicon and Intel macOS packages, verifies their checksums and audits the locked Rust dependencies before publication. Source builds still require a compatible VLC 3 installation. Linux OS media controls use the desktop D-Bus session; unavailable OS integration does not prevent local playback. Runtime-dependent tests are explicitly ignored by the default suite and run separately when VLC, codecs or network access are available.
