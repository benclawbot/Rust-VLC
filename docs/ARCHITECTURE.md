# Architecture proposal

Status: proposed, 2026-10-20. Rust implementation begins after visual selection. M0 must prove rendering before this becomes an accepted runtime architecture.

## Stack and structure

Use one Cargo package at the repository root. This is a standalone repository, so no extra `desktop/` project is needed. Separate modules along ownership boundaries rather than introducing workspace crates.

```text
Cargo.toml                 parent-owned dependency/features manifest
Cargo.lock                 committed for reproducible application builds
rust-toolchain.toml        stable version verified and pinned in T01
build.rs                   Slint build integration only
src/main.rs                launch, UI setup, coordinated shutdown
src/lib.rs                 modules exposed to focused integration tests
src/model.rs               IDs, owned records, shared enums
src/app.rs                 UI controller and orchestration
src/playback/mod.rs        public commands/events, serialized player owner
src/playback/ffi.rs        audited libVLC 3 types/symbols and RAII wrappers
src/playback/frame.rs      bounded frame publication and lifetime protocol
src/playback/queue.rs      pure queue transitions
src/library/mod.rs         scan orchestration and browse queries
src/library/scan.rs        traversal and generation/reconciliation
src/library/series.rs      filename recognition and natural episode order
src/storage.rs             SQLite connection owner and repository methods
src/metadata/mod.rs        metadata precedence and artwork jobs
src/metadata/tmdb.rs       optional movie/TV lookup and conservative matching
src/metadata/music.rs      embedded/folder art, later provider work
src/platform.rs            directories, native paths, runtime discovery
ui/app.slint               selected desktop composition
ui/components/             reused Slint controls, tokens, media cards
tests/                     behavior and integration fixtures
migrations/                ordered SQL migrations; storage owner only
packaging/                 Arch, Windows, macOS installation definitions
```

The tree is a target, not a request to create empty files. Add modules when their task needs them.

Use Slint for native UI; SQLite through a small Rust API for persistence; libVLC for decoding, demux, audio output, and playback. Prefer standard threads, channels, timers, and filesystem APIs. T01/T02 select and pin dependencies after verifying their current MSRV, platform support, and licensing. Candidate dependencies are `slint`/`slint-build`, `rusqlite`, `libloading` if dynamic loading wins the spike, `serde` where serialization is needed, a native file dialog crate, and an HTTP client with TLS. Add no async runtime unless the chosen network architecture requires it.

## Threads and ownership

The UI thread owns Slint windows, images, and view models. UI callbacks enqueue commands and return promptly. A serialized playback owner runs all libVLC control calls. A storage owner runs writes and migrations. Scan and artwork workers use bounded work queues. Do not create a worker per media file or per card.

libVLC event/video callbacks may run concurrently. They copy only the required owned data, publish it, and return. Never call libVLC, acquire a lock held across a libVLC stop/release call, mutate Slint, or unwind across the C ABI from a callback. Keep callback context and the loaded library alive until the runtime can no longer call into them. Document the exact ordering and prove it with stop/reopen/close tests against the chosen runtime.

For the first rendering experiment, publish a latest-owned-frame mailbox with at most a small fixed buffer pool. A UI timer consumes the latest frame and constructs a Slint image on the UI thread. Superseded frames are dropped; they are never queued indefinitely. Define pitch, byte order, alignment, checked allocation bounds, dimensions, and format renegotiation explicitly. Never expose a buffer to Slint while VLC can mutate it.

This CPU-copy path is an experiment. VideoLAN documents efficiency and hardware-decoding limitations for memory callbacks. Native HWND, NSView, and X11 embedding are alternatives to measure; X11 embedding is not evidence of native Wayland support. If M0 fails its gate, record results and compare alternatives before broad UI implementation.

Sources: [libVLC 3 playback and callback API](https://videolan.videolan.me/vlc-3.0/group__libvlc__media__player.html), [Slint image/thread model](https://docs.rs/slint/latest/slint/struct.Image.html), [UI-thread dispatch](https://docs.slint.dev/latest/docs/rust/slint/fn.invoke_from_event_loop).

## Shared contracts to freeze in T01/T04

These are contract requirements, not compiled APIs yet. The parent publishes actual signatures and examples before parallel consumers implement against them.

| Contract | Required semantics |
| --- | --- |
| `MediaId` | Stable local DB identity; independent of artwork, title, path display, or query order |
| Native path | `PathBuf` in memory; reversible OS-specific bytes/UTF-16 plus platform discriminator in storage; lossy text only for display |
| Media record | Kind, local identity, display title, size/mtime fingerprint, duration if known, availability, optional movie/episode/audio metadata |
| Player command | Load ordered items at an index, play, explicit pause, stop, seek milliseconds, volume, rate, track/subtitle selection, shutdown |
| Player event | Owned snapshot/event with generation, media ID, state, elapsed/duration, seekability, errors and track lists; no raw pointers |
| Queue item | Unique entry ID plus media ID; duplicate entries are allowed and addressable |
| Scan request/result | Root ID and generation; discovered entries; complete/cancelled/error/offline outcome; only complete traversal can reconcile absence |
| Artwork job/result | Media ID, metadata fingerprint, provider/query identity, source/confidence, dimensions, cache key; stale results are ignored |
| UI intent | IDs plus typed values, never widget-owned references sent to workers |

Use bounded command delivery with a defined busy result. Coalesce frequent time/volume/seek updates. Do not drop terminal state, error, end-of-media, or shutdown events. Associate each load with a generation so late events from a previous file cannot resume or advance the current queue.

## State and data model

SQLite is the source of library truth; UI models are projections. Enable foreign keys and serialize schema changes. Use transactions for migrations, scan batches, playlist reorder, and queue persistence. Back up an existing database before a migration; detect newer unsupported schemas and preserve the file instead of trying to downgrade it. Keep credentials outside the library DB and logs.

| Table group | Minimum data |
| --- | --- |
| roots, scan_runs | Native root path, enabled state, availability; generation and outcome |
| media, root_membership | Stable media identity; native path; file fingerprint; membership in overlapping roots; missing/offline state |
| series, episodes | Local series key, season/episode fields, overrides; provider IDs optional |
| audio metadata | Artist/album/album-artist/genre/disc/track; unknowns remain unknown |
| progress, history | Last observed position, duration, completion flag and history timestamps |
| playlists, playlist_items | Names, ordered entry IDs/media IDs, duplicates retained |
| queue_state | Ordered entries, current entry, shuffle/repeat state; restore paused |
| artwork_matches | Provider ID, candidate confidence, manual correction, metadata fingerprint and cache key |
| preferences | Theme, selected roots, view/sort, network opt-in; no tokens |

File moves do not automatically imply identity preservation in v1. Keep an unavailable record and allow explicit relink; do not merge two files on title alone. Overlapping roots must not duplicate records. Keep original native path spelling and filesystem-aware comparison; do not globally lowercase paths. Test Unix non-UTF-8 names and Windows UTF-16 edge cases on their native platform.

Scans traverse only user-selected roots, do not follow directory symlinks by default, skip app cache/data, report permission failures, and support cancellation. If any traversal fails, do not mark unseen members missing for that root. A disconnected drive is offline, not an empty library. Replacing a file at an existing path invalidates file-dependent metadata without deleting playlists/history by accident.

## Playback and queue behavior

Keep queue transitions as pure Rust logic with tests. Explicit Next/Previous follow the selected order; repeat-one applies to natural end-of-media, not manual Next. Repeat-all wraps; repeat-off stops at the end. Shuffle preserves a current entry and a stable shuffled traversal until the queue changes; Previous follows actual history. Define removal/reorder of the currently playing entry before implementation. One terminal event advances a load at most once. Playback errors surface the failing item and stop automatic advancement to prevent loops.

Persist resume every five seconds and on pause, stop, item switch, and coordinated exit. Restore stopped/paused rather than auto-playing. Offer Resume/Start over when a video position exceeds ten seconds and is below completion. Default video completion is 95% of known duration or natural end; test short/unknown-duration media separately. Never mark complete just because a user manually stopped. Music starts at the beginning by default; history may still record the track. All thresholds live in one module and remain documented.

## Metadata and artwork

Extract local tags through available VLC APIs first. Support embedded/folder audio artwork before online music services. Preserve original title/tags alongside display metadata and manual overrides. Filename cleanup must retain numeric titles and sequel numbers. Recognize conservative `S01E02` and `1x02` patterns with fixtures; unsupported multi-episode/date patterns stay unclassified until explicitly handled. Keep specials (season zero), numeric ordering, missing episodes, and ambiguous series names honest.

Online lookup is optional and off until the user enables it. Explain that title/year/artist queries leave the device; never send full paths. Use a user-supplied TMDB read token from an environment variable for development or an OS credential store for the application. A credential-store failure means online lookup stays unavailable, not a plaintext fallback. Do not embed a shared API token.

Rank candidates using local title/year/series information, reject conflicting sequels/years, require an evidence-tested margin for automatic matches, and offer manual selection/clear. Cache provider identity and query results separately from media identity. Fetch TMDB configuration for image base URLs/sizes. Restrict authorization to the API host and never forward it on redirects. Bound redirects, body/image dimensions, concurrent jobs, timeouts, and retries. Honor 429/Retry-After with capped backoff. Cache misses and transient failures differently.

Start with a 256 MiB disk artwork budget, 32 MiB decoded-image budget, and two HTTP jobs; make the values constants, then measure. Cache writes are atomic, filenames are app-generated keys, and eviction is confined to the app-owned cache. Cached art works with online lookup disabled. Preserve a manual match during rescan; reject late results for changed metadata. Poster, landscape backdrop, and square album art are distinct cached variants.

## Platform boundary

Use platform data/config/cache directories; never the working directory or media folders. Runtime discovery accepts an explicitly configured absolute installation directory, then the app bundle, then documented platform locations. Never load a DLL from an arbitrary current directory. Probe architecture, version and required symbols before creating a player; report missing runtime/plugins with actionable setup text. Validate plugin discovery with actual media, not just `libvlc_new`.

Arch initially uses system libVLC and explicit playback plugin packages. Windows and macOS development may use a compatible installed VLC runtime. Distributable bundles must carry a tested runtime/plugin layout with an exact manifest, checksums, notices, and source/license obligations reviewed before release. Support claims require tests on the target OS and architecture. Signing/notarization credentials remain external to Git.
