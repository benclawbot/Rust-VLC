# Validation and release gates

Current baseline contains documents and HTML previews. No Rust, libVLC playback, Arch installation, macOS package or Windows package has been validated yet. Future commands below become runnable after T01; they are not current test results.

## HTML proposals (D00)

Serve the repository locally on loopback. Confirm that all fonts/images come from design/assets/ and that no external request is needed. Inspect Cinema, Archive and Spotlight at 1440×1000, 1024×768 and 800×900, including empty search and open title details. Check no body horizontal overflow, clipped primary action or broken image. Exercise search/filter/sort where present, navigation, title selection, My list, simulated Play feedback, series episodes, Esc and focus return. Test using keyboard and reduced motion. Preview sample content must be labelled.

Each proposal exposes `window.previewSelfCheck()` for a small assert-based data/behavior check. This supplements browser interaction; it does not replace it. Browser automation should use the environment's approved browser tools, with screenshots inspected visually. The source reference was inspected as a phone screenshot and CSS; a desktop redesign is not a pixel clone.

Archive's selected music extension also requires Movies/Music switching, artist/album/track/genre navigation, album track selection, queue display, previous/next and preview play/pause. Check that changing the visible library retains the selected mini-player track, square art stays square and the player does not cover the final content row. All audio feedback must remain labelled as simulated. Native audio release acceptance still requires real MP3/FLAC decoding, transport, queue advancement and playlist restart persistence.

## Automated Rust checks (after T01)

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build --release --locked
```

Separate runtime/display-dependent checks with an explicit test name or feature decided in T01. Keep pure filename, queue, scan reconciliation and SQLite tests runnable without libVLC or network access. Do not introduce an architecture solely to satisfy a test harness. Dependency/security review checks belong in CI once the dependency graph exists; pin any added tooling and document its version.

Fixtures are synthetic or redistributable with provenance and hashes. Store tiny text/JSON/SQL fixtures in tests/; generate or fetch large media under ignored artifacts/ with checksums. No credentials or private media paths in committed reports. Network tests use local recorded HTTP responses by default; real provider checks are explicit opt-in integration tests.

## Minimum automated cases

| Area | Required cases |
| --- | --- |
| Paths | Native round-trip, spaces, Unicode, long paths; Unix non-UTF-8 and Windows UTF-16 cases on native OS |
| Runtime/FFI | Missing/wrong ABI/architecture/symbol/plugins, null/error returns, paired frees, teardown with callbacks in flight |
| Queue | Empty/one/many, duplicates, reorder/remove current, repeat modes, shuffle history, late/duplicate terminal events |
| Scanning | Repeated/overlapping roots, cancellation, partial traversal, offline drives, symlink loops, replacement, permission errors |
| Persistence | Reopen, migrations from earlier schema, rollback/future schema, locked DB, ordering, settings and history retention |
| Series | S01E02/1x02, specials, numeric order, ambiguous title/year, numeric films, quality suffixes, unsupported multi-episode patterns |
| Artwork | Ambiguity/wrong year/sequel, manual override, stale results, 401/404/429/5xx, backoff, offline cache, atomic writes, bounded images |
| Audio | Tags/unknowns, album-artist conflicts, disc/track order, corrupt embedded art, folder precedence |
| UI adapter | Actual state/events, no stale title after load, stable focus/scroll during updates, missing-art/offline/error states |

Memory/undefined-behavior tools are useful where available, but Miri does not establish safety of a real native VLC runtime. Document unavailable sanitizer support instead of claiming equivalent native coverage from mocks.

## Native matrix

| Target | Required display/runtime evidence | Packaging evidence |
| --- | --- | --- |
| Arch x86_64 | Wayland and X11, selected Slint renderer and system libVLC/plugin versions | Clean install/upgrade/uninstall, desktop launch, missing runtime/plugins |
| Windows x86_64 | Native Slint window, tested VLC runtime, monitor scaling, audio/subtitle/video | Installed launch with no developer PATH/VLC dependency, upgrade retention, signing status |
| macOS arm64 | Native Slint window, matching VLC runtime, Retina scaling, audio/subtitle/video | App launched outside checkout, runtime loader paths, upgrade retention, signature/notarization status |
| macOS x86_64 if offered | Separate runtime/build/playback checks | Separate tested package; no inferred universal support |

A Windows development host cannot close Linux/macOS gates. CI compile success cannot close real display/GPU/audio tests. Platform credentials can block signed distribution while unsigned development smoke checks still proceed; label them separately.

## Real playback checklist

1. Record source SHA, OS version, architecture, display server, renderer, GPU/driver, CPU/RAM, Rust/Slint/libVLC versions, plugin manifest and fixture hashes.
2. Open local MP4 H.264/AAC, MKV with selectable tracks/subtitles, MP3 and FLAC. Include a corrupt file and a disconnected root. Scope codec claims to actual tested formats.
3. For M0, play 1080p30 for ten minutes on Arch Wayland and X11. Measure presentation drops, CPU and resident memory after warm-up. Target less than 1% dropped presentation frames and no sustained memory growth. Record methodology and the bounded mailbox behavior; inspect actual colors/aspect and listen for A/V sync.
4. Pause/resume, seek forward/back, change volume, resize, toggle fullscreen and close while playing. Repeat open/stop/reopen/close twenty times. Confirm no callbacks touch released state and no thread/process remains after exit.
5. Queue two videos and two audio files, exercise manual and natural transitions, repeat/shuffle, duplicate entries and unavailable items. Confirm the UI and actual media agree.
6. Add local subtitles, switch/disable tracks, adjust rate/sync, open chapter/time controls and test unavailable operations.
7. Reopen the app: roots, metadata, playlists, overrides, preferences and history persist; restored queue is paused; Resume/Start over behaves as specified.
8. Disable online artwork and network access: cached art and local playback still work. Trigger invalid token and 429 through local response tests; confirm no token or full local path leaks into logs.

4K60, HDR, passthrough, DRM, optical discs, cast and network protocols require dedicated evidence before being advertised. They are not implied by the use of VLC.

## Release evidence template

For each task/milestone record status (not started/in progress/blocked/accepted), base/head SHA, changed files, exact commands and exit results, native matrix rows exercised, screenshots or redacted reports, unresolved findings, and reviewer model/effort. A checklist tick must link to actual evidence.

M4 additionally requires checksums, exact runtime/plugin inventory, source/dependency/license notices, no secrets or sample media in the package, clean first launch, upgrade with an existing database, cache/data isolation, and uninstall behavior. Do not remove the user's source media or database during uninstall without explicit user intent. Publish only after the release task is authorized.
