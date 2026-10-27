# Validation and release gates

## 0.1.3 verification (2026-10-27)

The final source passed `cargo test --all-targets --locked` (202 passed, eight ignored), `cargo clippy --all-targets --locked -- -D warnings`, formatting and diff checks. `cargo audit --file Cargo.lock` reported no vulnerabilities and 11 allowed unmaintained-dependency warnings. Independent GPT-6.1 Sol xhigh source review checked the lock, installation scripts, TMDB handling, relinking and playback transitions; identified issues were repaired and re-reviewed. The release workflow includes clean install, 0.1.2 upgrade and database retention checks on disposable runners. Native graphical playback and provider behavior require separate verification; these are not established by the automated tests.

All eight ignored runtime checks passed serially on Windows with libVLC 3.0.24, including video-frame decoding and the live Cover Art Archive check. A parallel run timed out in three playback cases; those cases passed in the serial run. These checks do not replace hands-on UI or installed-platform testing.

## 0.1.2 release (2026-10-27)

Version 0.1.2 is published at the [GitHub release](https://github.com/benclawbot/Rust-VLC/releases/tag/v0.1.2). It combines the review fixes with queue/root persistence and the approved P01/P02/P04 features; parental/PIN locking and paired LAN remote control were explicitly excluded. Implementation used GPT-6 Luna Max workers. Independent GPT-6.1 Sol xhigh review of the frozen repaired source found no remaining actionable findings in scope. This is source-level review, not installed native-platform acceptance.

Tagged run [40495482085](https://github.com/benclawbot/Rust-VLC/actions/runs/40495482085) built the Arch, Windows, Apple Silicon and Intel macOS installers, passed the dependency audit and published all four packages with their checksum sidecars. All eight published asset digests match independently downloaded CI files, and tag `v0.1.2` points to source commit `6eb46e5f3d79462021d1c785859f7de73d6888f8`. The Arch package was inspected for the executable, launcher, icon, version and build provenance. Both macOS packages contain the app, icon, matching libVLC runtime and respectively 335 and 341 VLC plugins; Windows Setup contains icon resources. These checks do not substitute for clean installation, upgrade, uninstall or native playback on every platform.

The final `cargo test --all-targets --locked` run passed **181 tests, zero failures, eight ignored**. The new mixed-playlist regression first exposed another Windows short-path alias duplication; the importer now resolves both stored and canonical identities without rewriting paths, and the original ID/order assertion passes. Independent review also drove repairs for desired-state Play/Pause, PiP wiring, Unicode persistence, mixed-playlist visibility/video launch and removing the current restored queue item without autoplay. Strict Clippy (`--all-targets --locked -- -D warnings`), formatting, diff checks and both workflow YAML parses pass. The repaired debug application build passed in 28.63 seconds. The final Windows release build with `RUSTFLAGS=-C target-feature=+crt-static` passed in 7m07s; its 0.1.2 version/icon resources and absence of VCRUNTIME/MSVCP imports were verified.

Native Windows inspection decoded the synthetic H.264 movie and confirmed that centered transport controls hide during playback. The fresh [Movies](screenshots/movies.jpg) and [Music](screenshots/music.jpg) captures show the 0.1.2 library. Stream tools are reachable before loading media. The repaired debug app was relaunched with an isolated fixture: its saved queue showed Ready without autoplay; double-clicking a movie entered native borderless fullscreen, with no title bar in the accessibility tree and only the centered three transport buttons. Source aspect was preserved with letterboxing on the ultrawide display. PiP and complete native shortcut/settings flows still need hands-on qualification.

All eight default-ignored cases were selected explicitly and passed. Seven native checks used the installed VLC 3.0.24 runtime: HTTP 404 failure stays on the first item under Repeat All and no signed-token marker appears in captured stdout/stderr (62.53 seconds); seek/volume/rate/stop and equalizer settings (17.99 seconds together); stale-seek rejection, missing-next handling and replay/repeat advancement (7.67 seconds together); and generated video-frame decoding with FFmpeg (6.79 seconds). The live Cover Art Archive redirect/image check also passed (2.84 seconds).

The HTTP fixture separates a bounded 120-second runtime initialization from the eight-second HTTP deadline, after an earlier attempt timed out before `runtime_checked`. Explicit checks used a copied test harness compiled immediately before the final M3U8 identity-only repair; playback/FFI/artwork code was unchanged by that repair. The ordinary suite still reports eight ignored; the explicit results above are separate evidence. Real Chromecast, authenticated subtitle-provider accounts, live YouTube matching, all codecs and macOS/Arch native playback were not tested.

The refreshed RustSec audit identified RUSTSEC-2026-0385 in ctutils 0.4.3 and RUSTSEC-2026-0384 in Tokio 1.54.0. The lockfile now uses ctutils 0.4.4 and Tokio 1.54.1; yanked clipboard-win 5.4.1 was updated to 5.4.3. `cargo audit --file Cargo.lock` then exited 0 with no reported vulnerabilities and 11 upstream maintenance warnings across 726 dependencies. The workflows now audit the locked graph before release publication.

The earlier GitHub Actions preflight run `39696328297`, publish job `121489413738`, was refused before runner assignment because of account payments/spending limits. Tagged run [40476658676](https://github.com/benclawbot/Rust-VLC/actions/runs/40476658676) started all four platform jobs and passed its dependency audit, so the billing block cleared. Linux and macOS ARM exposed a missing `Path` reference in a Unix-only test; the reference and platform-specific warnings were corrected. The next run [40491145000](https://github.com/benclawbot/Rust-VLC/actions/runs/40491145000) compiled on Linux and macOS ARM, then exposed a real-window test on headless runners and a Windows-only path fixture. The callback test now uses a test-local Slint software window, and the playlist fixture uses native paths; the final tagged build passed both changes.

Automatic command review rejected local portable Inno Setup setup with `blocked by policy`; no local compiler was installed. The hosted Windows job installed the compiler and built the new installer successfully. Native installed Arch/macOS rendering and real receiver/provider checks remain separate gates.

Run [40493078408](https://github.com/benclawbot/Rust-VLC/actions/runs/40493078408) passed Linux and Apple Silicon tests/builds, but inspection of the uploaded Arch artifact revealed a debug-symbol package renamed as the installer: its checksum was correct for that wrong file, and it contained no `usr/bin/rust-vlc`. The run was cancelled before publication. The packaging recipe now disables the separate debug package, selects the exact runtime package name, and checks package identity, required files and source build information before upload. Independent checks rejected that bad package and accepted the real v0.1.1 package with matching identity. The final run built a correct 0.1.2 runtime package, which was independently inspected after download.

Implementation review: 2026-10-21. The native app now includes movie/video playback, a persistent local library, music metadata/artwork, ordered playlists and player tools. The pinned Rust 1.95.0 toolchain is installed on the Windows x86_64 host; libVLC 3.0.24 was used for playback validation. The final integrated checks and native UI evidence are recorded below. Development archives and compilation do not close native platform or installation gates.

The HTML preview was manually checked for playlist creation from selected tracks, choosing an existing playlist, adding multiple checked tracks in displayed order, shuffling an opened playlist's stored order, clicking a shuffled song to start its matching preview, and rejecting a duplicate add while keeping keyboard focus on the Add button. Shuffling an equal-content playlist leaves the unrelated active queue intact; shuffling its owning playlist updates the queue while preserving the current song. At 1887×1096, the page has no horizontal overflow, every visible image loaded, and the browser reported no warnings or errors. Playback in this preview is simulated and produces no audio.

Independent Sol xhigh review covered frozen source lots, including callback ownership, queue occurrences, migrations, path encoding, provider boundaries and the original Première collection/playlist/playback flows. Luna Max workers implement the fixes. [UX_PARITY_REVIEW.md](UX_PARITY_REVIEW.md) records the behavior comparison and explicit gaps. Captured Android screens and source inspection are evidence of the reference experience, not a new APK test.

The dependency update checked stable versions against crates.io, moved reqwest from 0.12.28 to 0.13.5 with its explicit blocking/query/rustls features, and refreshed compatible transitive versions. Other direct dependencies were already current. Reqwest's TLS backend also requires CMake when building AWS-LC. `cargo audit --file Cargo.lock` with cargo-audit 0.22.2 refreshed RustSec and scanned the updated 634-dependency lockfile: exit 0, no reported vulnerability, six unmaintained warnings (`bincode`, `paste`, `ttf-parser`, `zbus_names`, `zvariant`, `zvariant_derive`). These upstream maintenance warnings remain recorded; the audit is not proof of memory safety or of a clean native runtime.

## Integration review evidence (2026-10-21)

The 2026-10-21 integrated checks on Windows x86_64 ran `cargo fmt --all -- --check`, `cargo check --all-targets --locked`, `cargo test --all-targets --locked` (119 passed, 6 ignored), and `cargo clippy --all-targets --locked -- -D warnings`; all passed after the player-tools and mini-player icon update. Six runtime/network tests remain explicitly ignored by the default suite. The initial test run exposed an incorrect one-based expectation in the later-page playlist fixture; its zero-based occurrence assertions were corrected and the suite rerun. Later review also added coverage for automatic-art retry ordering and rejecting live/cover variants from YouTube music matches.

The Windows `cargo build --release --locked` build passed. A second Windows release build with `RUSTFLAGS=-C target-feature=+crt-static` also passed; PE import-table inspection confirmed the executable no longer imports `VCRUNTIME` or `MSVCP` DLLs. The tagged-release workflow uses the same flag for Windows tests and builds. For v0.1.1, release packaging pins VideoLAN VLC 3.0.24 downloads by SHA-256, includes the full Windows VLC directory and macOS VLC.app runtime, and generates Windows/macOS/Linux icons. The Linux release is an Arch package with direct libVLC and full plugin dependencies and an app-launcher entry. The workflow YAML parses, and the downloaded Windows runtime archive matched its pinned SHA-256 and contained `libvlc.dll`, `plugins`, and `COPYING.txt`.

## Installer refresh checks (2026-10-22)

On Windows x86_64, `cargo +1.95.0 fmt --all -- --check`, `cargo +1.95.0 clippy --all-targets --locked -- -D warnings`, and `cargo +1.95.0 test --all-targets --locked` passed (122 passed, 6 ignored). The ignored cases require optional VLC runtime, codecs, or network access. The native VLC 3.0.24 playback integration test for seek, volume, rate, and stop also passed. The release workflow YAML parses, its four platform matrix entries are present, and `git diff --check` passes. Independent GPT-6.1-Sol xhigh review found no remaining actionable source-level blockers after fixing macOS runtime selection and packaging paths.

The v0.1.1 tagged workflow run `39696328297` built all four packages successfully: Arch x86_64, Windows x86_64, macOS Apple Silicon and macOS Intel. All four downloaded installer files passed SHA-256 verification against their sidecars. GitHub blocked the workflow's publish job from starting because the account hit its Actions billing/spending limit; the verified installers and checksums were then uploaded to the published v0.1.1 release through the authenticated GitHub CLI. The release is at [github.com/benclawbot/Rust-VLC/releases/tag/v0.1.1](https://github.com/benclawbot/Rust-VLC/releases/tag/v0.1.1).

CI package creation passed on all four native runners. Clean installed launch/playback, upgrade and uninstall behavior remain unverified on all three OS families; Arch and macOS installed checks could not be run on this Windows host. The macOS packages are unsigned and not notarized.

Frozen-source Sol xhigh review found and verified fixes for model identity during UI refresh, same-item resume ownership, asynchronous VLC startup readiness, and Windows canonical path handling. The isolated playback harness passed 12 focused unit tests, three native resume tests and five prior native regressions with the installed libVLC 3.0.24 runtime. The resume tests repeatedly replaced the same item, queued different resume positions rapidly, and stopped a pending resume before a fresh play.

The native GUI exposed a Windows import failure that ordinary-path fixtures had missed: VLC interpreted `\\?\C:\...` as `file://?/C%3A/...` and immediately ended playback without frames. After the boundary fix, both direct-engine and playback-worker tests passed for ordinary paths (2 tests) and canonical imported paths (2 tests), decoding frames and retaining the expected 60-second duration and first queue item. Stored paths were not rewritten. Actual UNC shares and native long-path playback remain unverified.

The final custom-control review corrected dropdown selection/scrolling, long-text caret visibility, slider cancellation/focus loss, checkbox focus, slider arrow-key routing and Space activation on transport buttons. Right-click text-edit menus remain a usability gap. The latest visual refinement removes the raw media path from the player-tools panel, hides an empty chapters column, centers the responsive tool groups with dividers, and uses compact icons for clear actions. Stopped playback also presents Play instead of Pause. The Slint screen compiles, but this session cannot inspect a fresh native capture. Existing screenshots document the earlier native layout and need recapture before being called current.

The final independent GPT-6.1 Sol xhigh review found no remaining actionable findings. It confirmed the Windows runtime import fix, the 853-pixel player-tools layout with chapters at the 900-pixel minimum, and the correct Play/Replay/Pause state labels. The reviewer did not verify native rendering, clean-system install/upgrade, or tagged CI packaging.

The Windows x86_64 debug app was inspected at 1920×1152 with an isolated synthetic library; [Movies](screenshots/movies.jpg) and [Music](screenshots/music.jpg) are native Rust/Slint captures, not HTML mockups. The player was launched from a movie card into borderless fullscreen with the three transport controls centered, and Escape returned to the library. The same synthetic sample set includes H.264 test-pattern video, tagged FLAC tracks and local artwork. These checks validate the Windows display path only; they do not establish multi-hour playback, all codecs, audio fidelity or another operating system's renderer.

## Selected HTML reference (D00)

Serve the selected `design/proposals/archive.html` locally on loopback. Confirm that fonts/images come from design/assets/ and that no external request is needed. Inspect desktop and narrow sizes, including empty search and title details. Check no horizontal overflow, clipped primary action or broken image. Exercise search/filter/sort where present, navigation, title selection, My list, simulated Play feedback, series episodes, Esc and focus return. Test keyboard and reduced motion. Preview sample content must be labelled.

The preview exposes `window.previewSelfCheck()` for a small assert-based data/behavior check. This supplements browser interaction; it does not replace it. Browser automation uses the environment's approved browser tools, with screenshots inspected visually. The source reference was inspected as phone screenshots, Java and CSS; the desktop redesign preserves its visual language without copying its phone layout.

Archive's selected music extension also requires Movies/Music switching, artist/album/track/genre navigation, album track selection, queue display, previous/next and preview play/pause. Check that changing the visible library retains the selected mini-player track, square art stays square and the player does not cover the final content row. All audio feedback must remain labelled as simulated. Native audio release acceptance still requires real MP3/FLAC decoding, transport, queue advancement and playlist restart persistence.

## Automated Rust checks

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo check --all-targets --locked
cargo test --all-targets --locked
cargo build --locked
cargo build --release --locked
```

Final command outcomes are recorded with the delivery evidence below. Runtime- and network-dependent tests are ignored in the default suite and must be explicitly selected; ignored does not mean passed. One cold libVLC initialization exceeded the old five-second test deadline during review, then passed on a warm rerun; initialization deadlines and actual runtime failures must be distinguished.

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
