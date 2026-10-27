# Architecture

Implementation snapshot: 2026-10-26, source version 0.1.2. Rust-VLC is one Rust package with a Slint interface and a dynamically loaded libVLC 3 runtime. The expanded implementation is undergoing integrated review; native Arch Wayland/X11 and macOS playback qualification remain open. See [validation](VALIDATION.md) for evidence rather than inferring support from compilation.

## Modules and dependencies

| Module | Responsibility |
| --- | --- |
| `app.rs`, `ui/app.slint` | UI-thread models, user intents, collection navigation, progress and worker orchestration |
| `playback/mod.rs` | Serialized playback owner, commands, snapshots, transport and queue editing |
| `playback/ffi.rs` | libVLC 3 ABI, loaded library lifetime, owned player/media/list resources and callbacks |
| `playback/frame.rs` | Bounded owned RGBA frame transfer to the UI thread |
| `playback/queue.rs` | Pure queue traversal, duplicate occurrences, repeat and shuffle |
| `model.rs`, `library.rs` | Native-path media records and displayed-row selection |
| `scan.rs`, `library_worker.rs` | Bounded background folder traversal, metadata and artwork jobs |
| `browse.rs`, `series.rs` | Music grouping and conservative filename-based series/season ordering |
| `metadata.rs`, `credentials.rs` | Lofty tags, local art, bounded provider matching/cache and OS-backed TMDB credentials |
| `subtitles.rs` | Bounded OpenSubtitles search/download, host/authentication checks, cancellation and validated subtitle cache |
| `media_controls.rs` | UI-thread-owned OS media bridge with bounded owned callback events and cached metadata updates |
| `storage.rs` | SQLite schema, migrations, native-path encoding and coalesced writes |
| `platform.rs` | Runtime discovery from explicit installation and documented system locations |

Rust and dependencies are pinned by `rust-toolchain.toml`, `Cargo.toml` and `Cargo.lock`. Slint 1.18.1 owns the window; libloading loads libVLC; rusqlite uses bundled SQLite; Lofty reads tags; reqwest uses blocking requests with rustls on a background thread; image validates and resizes artwork; rfd supplies native file dialogs. There is no production webview or local web server.

## Ownership and shutdown

The UI thread owns every Slint window, model and image. UI callbacks send bounded commands; a timer observes owned playback snapshots. A separate short timer consumes the latest video frame. No Slint object crosses into playback, scan or storage threads.

One playback thread owns libVLC control calls. Callbacks do no libVLC calls and do not unwind across the C ABI. Decoding uses a small preallocated, aligned plane pool with checked dimensions and a strict per-frame allocation limit. Unlock copies a decoded picture into owned pending storage and returns its plane; the display callback publishes the picture at libVLC's presentation time. Pending storage and the latest-frame mailbox are bounded. Cleanup closes the callback context and waits for in-flight callback access before freeing it; the player is released before the runtime library.

The library worker has four pending jobs and 32 results. Automatic music artwork runs on a second worker with eight pending requests, so network lookup does not hold the UI thread or the worker's scan operation. Local metadata is published before an artwork request is queued; backpressure keeps eligible tracks from being silently dropped. Scans cap entries, nesting and warnings, skip symlinks and check cancellation between filesystem operations. Dropping the result receiver releases blocked publishers before join. Artwork cancellation prevents subsequent provider requests/redirects; an active request can still take its ten-second timeout, plus up to one second for provider rate limiting. A single operating-system filesystem call may still block.

Storage receives the latest owned library snapshot through a coalescing mailbox. It writes on its own thread, reports errors to the UI and flushes on coordinated exit. Exit joins workers; it does not delete source media.

## Playback contracts

Commands and snapshots contain owned Rust data, never player pointers or widget references. A load generation distinguishes successive plays of the same media, including duplicate playlist occurrences. Media-scoped controls and deferred seeks validate that generation. `PlayQueue` carries an optional initial resume position; the playback owner binds that seek to the new load, so a stale UI snapshot cannot consume it against an earlier play of the same movie. Queue edits additionally validate a queue revision and traversal position.

Queue traversal preserves duplicate occurrences. Repeat-one applies only to natural completion; manual Next still advances. Repeat-all wraps. Playback shuffle changes traversal without changing saved playlist order. Stored playlist shuffle changes that playlist's order and updates its owning active queue without reloading the current song; unrelated queues stay intact.

Clicking a song plays its displayed collection at the clicked occurrence. Leading checkboxes never play media. Select all, Play selected and Add to playlist use displayed order. Movies and Music share one playback owner, so changing pages does not stop playback. Double-click starts a movie fullscreen; Escape leaves fullscreen without stopping it.

Movie progress is periodically saved and captured on transport changes and exit. Resume is offered beyond ten seconds and below 95% completion; Start over plays from the beginning. Explicit new music selections start from the beginning. A queue checkpoint retains base entries, shuffled traversal, the selected duplicate occurrence, wrap state, repeat mode and position. Startup restores a ready queue without calling VLC Play; an explicit Play binds the saved position to its new load. An empty startup snapshot cannot replace a disk checkpoint before restoration is acknowledged.

Returning to fullscreen and picture-in-picture reuse the same playback owner and load. The UI thread shares the latest decoded frame between windows; screenshots save packed RGBA pixels as PNG. Equalizer operations use libVLC's actual bands/presets and persist settings. A–B markers are load-scoped; sleep timers and cast targets are not restored automatically. Casting configures VLC's Chromecast output for a validated manual address and does not claim receiver confirmation.

## Persistence and native paths

Schema version 4 stores media, local metadata/artwork overrides, ordered playlist entries, movie progress, watched roots, queue checkpoints, bookmarks, network history and preferences including equalizer settings. Duplicate playlist entries use their persisted positions. Album grouping uses album artist when available, retaining compilations with different performers; opened albums default to disc/track order.

Native paths remain `PathBuf` in memory and are stored as Unix bytes or Windows UTF-16LE with an encoding discriminator. Display text may be lossy; playback paths are not reconstructed from it. Databases are platform-specific, not a cross-platform library interchange format.

`TrackSource` distinguishes local files from validated network addresses. Network entries use a separate URL encoding in storage and `libvlc_media_new_location`; URLs are never substituted for native paths. HTTP, HTTPS, RTSP and RTMP require a host and reject userinfo/control characters. Signed query strings remain private in local storage for replay, while display labels and errors redact them. M3U8 interchange preserves duplicate entries and supports local references and validated network sources.

At the libVLC boundary, recognized Windows verbatim drive and UNC prefixes are adapted to the paths VLC accepts; stored paths remain untouched. Unsupported device namespaces are rejected. The current libVLC path interface requires UTF-8; Unix names that cannot be represented in UTF-8 produce an explicit playback error rather than a lossy substitution.

Transactions protect schema upgrades, library snapshots and playlist ordering. Recognized upgrades back up the existing database first. Future or unrecognized schemas are preserved and rejected. Imports are additive: an incomplete or disconnected scan never removes unseen media or history.

Watched roots persist as native paths and are scanned at startup, periodically and on request through the bounded library worker. Scan events carry their root and file size/modified-time fingerprints. Changed files can refresh metadata without replacing their media identity. Only complete scans reconcile availability; missing or offline files retain their library entries, playlists and history. Removing a root stops watching it and keeps imported entries and source media. Fingerprints use filesystem metadata rather than content hashes.

## Artwork and privacy

Local metadata uses filename fallback, embedded artwork, then named nearby cover/folder/poster images. A manual artwork override survives later metadata reads.

TMDB artwork starts through the explicit movie/TV artwork action. The token comes from the system credential store, with `TMDB_READ_TOKEN` retained as a fallback for existing setups; it is sent only to the TMDB API host. Automatic music artwork starts after local metadata loading when embedded and nearby art are absent. It searches public YouTube results with the tagged track title and artist, accepts only strong matches, and falls back to MusicBrainz/Cover Art Archive, which may also receive the album tag. This uses a bounded parser for public `ytInitialData` and does not run page JavaScript or use sign-in cookies. Requests never contain native media paths.

Authorization is confined to TMDB's API host. Redirects and provider hosts are validated. Responses, dimensions, decoded memory, disk cache, request duration and job counts are bounded. Matching rejects ambiguity; positive results and temporary negative results are cached separately. Downloaded art is validated and reduced to thumbnails, with a 256 MiB cache budget. The UI also bounds loaded artwork and visible rows.

Artist and genre cards derive artwork from matching local tracks; they do not claim dedicated artist photography. Movie genre metadata is not yet populated from TMDB search results, so movie genre filtering depends on genre data already stored with a library item. Landscape backdrops and a provider-candidate picker remain follow-on work. The TMDB token is stored through the native Windows Credential Manager, macOS Keychain or Linux Secret Service; on Linux the user session must have a Secret Service provider running. Error messages do not include credentials.

## Development executables

The native-runner workflow produces Linux x86_64, Windows x86_64, macOS arm64 and macOS Intel archives, each with a source commit and checksum. VLC and plugins are installed separately and must match the executable architecture. Windows uses a GUI subsystem and reports fatal startup errors in a dialog. macOS archives contain unsigned app bundles.

Development archives are distinct from signed, installed release packages. [Runtime setup](../packaging/RUNTIME.md) and [validation](VALIDATION.md) record the remaining platform, packaging and licensing gates.
