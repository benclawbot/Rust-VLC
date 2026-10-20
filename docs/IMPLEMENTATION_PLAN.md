# Implementation plan

Planning baseline: 2026-10-20. The approved targets are Arch Linux, macOS, and Windows. The application is local-first; online artwork is optional. The first release includes video/series, music, ordered playlists, history, and packaging. Android-only services are replaced with desktop equivalents.

## Gates and status

| Gate | Deliverable | Current status |
| --- | --- | --- |
| D0 | Three HTML proposals, browser checks, user selection | Accepted: Archive Movies/Music selected and revised; playlist interactions checked |
| M0 | Slint/libVLC feasibility with measured real playback | Not started: Arch Wayland/X11 video rendering is untested |
| M1 | Arch desktop player with functional transport and queue | In progress: Windows audio slice exists; Arch acceptance is untested |
| M2 | Persistent movie/series library and optional artwork | Not started |
| M3 | Music, playlists, resume, player tools, settings | In progress: local tracks and session-only playlists exist; persistence and metadata are unimplemented |
| M4 | Tested installation/upgrade packages for all three OS families | Not started |

A gate closes only with its evidence in docs/VALIDATION.md. The first native audio slice is implemented, but no release milestone is accepted; a Windows audio smoke test does not close Arch playback or rendering gates.

The critical path remains **D00 selection → T01 → T02/T03 → M0 review → T04/T05/T07 → integrated player → library/artwork → music/playlists → installed release checks**. The first audio shell provides a starting point; broad video/library work still depends on measured rendering evidence. Backend tasks can overlap after contracts are published.

## Task catalogue

Each task is intended to fit a focused worker change. If it cannot be reviewed as one coherent change, split it and preserve its acceptance criteria. Estimates should be recalibrated after M0; the earlier 1–2 week / 2–4 month figures are not delivery commitments.

| ID | Work | Dependencies | Exclusive implementation paths |
| --- | --- | --- | --- |
| D00 | HTML proposals, selection and design contract | None | `design/` |
| T01 | Build foundation and shared model contracts | D00 selected | Cargo files, `build.rs`, `src/{main,lib,model,platform}.rs`, basic CI |
| T02 | libVLC runtime discovery and ownership | T01 | `src/playback/ffi.rs`, runtime diagnostics tests |
| T03 | Embedded-rendering feasibility | T02 | `src/playback/frame.rs`, minimal spike UI, feasibility report |
| T04 | Publish command/event/storage interfaces | T03 accepted | Parent owns shared contracts and module wiring |
| T05 | Player controls and event state | T04 | `src/playback/mod.rs`, playback integration tests |
| T06 | SQLite schema and migrations | T04 | `src/storage.rs`, `migrations/`, storage tests |
| T07 | Selected Slint desktop shell | T04 | `ui/`, `src/app.rs`, UI interaction checks |
| T08 | Safe root scanning and library browse/search | T06 | `src/library/{mod,scan}.rs`, scan fixtures/tests |
| T09 | Queue/transport transitions | T05 | `src/playback/queue.rs`, queue tests |
| T10 | Movie identity and TV series/seasons | T08 | `src/library/series.rs`, filename fixtures/tests |
| T11 | Optional TMDB matching and artwork cache | T06,T10 | `src/metadata/{mod,tmdb}.rs`, artwork fixtures/tests |
| T12 | Resume/history/queue persistence | T06,T09 | `src/history.rs`, history tests; schema edits via storage owner |
| T13 | Audio metadata, grouping, local artwork | T08,T05,T09 | `src/metadata/music.rs`, audio fixtures/tests |
| T14 | Ordered playlists | T06,T09 | `src/playlists.rs`, playlist tests |
| T15 | Tracks/subtitles and player tools | T05,T07 | Playback tool module and isolated UI tool components |
| T16 | Optional online music artwork | T11,T13 | `src/metadata/music_online.rs`, provider fixtures/tests |
| T17 | Settings, themes, diagnostics, privacy/About | T07,T11,T13 | Settings module/panel, preference tests |
| T18 | Arch installation package and desktop entry | M1,T17 | `packaging/arch/`, install test report |
| T19 | Windows runtime bundle and installation | T18 | `packaging/windows/`, Windows-specific platform code |
| T20 | macOS bundle, runtime, signing/notarization | T18 | `packaging/macos/`, macOS-specific platform code |
| T21 | Cross-platform release verification | T12,T13,T14,T15,T17,T18,T19,T20; T16 only if included | CI/release workflows, license/runtime inventory, release report |

Schema changes from any feature must be coordinated with T06's owner. No parallel worker edits central migrations, Cargo.toml, model.rs, or app.rs without an ownership transfer. T07's owner or parent integrates feature views after the corresponding backend review.

## D00: visual selection

Deliver three HTML proposals with the same Première font/palette, different movie access patterns, functional local search/title details, and explicit sample/preview labels. The user selected Archive and required audio: remove proposal navigation from the selected app view, put a prominent Movies/Music switch across the main top bar, and provide poster art on album, artist and genre cards plus matching cover thumbnails on track rows. Music includes artist/album/track/genre/playlist views, album tracks, bulk adding checked tracks to a new or selected playlist, a queue and a persistent preview mini-player. Inspect desktop and narrow sizes, keyboard focus, detail-dialog dismissal, empty results, playlist flows and image/font loading. Record the selected file/commit and requested adjustments in design/README.md; treat that snapshot as the target for T07. A screenshot of a preview is not a functioning player.

## M0: eliminate playback uncertainty

### T01: foundation

Create one Cargo package, minimum Slint window, native file selection, and development instructions. Verify and pin a stable Rust version and compatible Slint/build versions; commit Cargo.lock. Record dependency/license choices, app ID, data/cache/config directories, minimum OS versions and CPU architectures as explicit decisions. Publish model IDs, path representation and worker result types. Initial target: Arch x86_64, Windows x86_64, macOS arm64; evaluate macOS x86_64 as a separately tested target rather than implying universal support.

Acceptance: format/check/test commands work from a clean checkout; the window opens on the active platform; headless tests do not require VLC or display access; absent runtime produces setup guidance. CI compiles/tests Linux, Windows and macOS where runners permit. Confirm actual installed toolchain compatibility rather than assuming the planning host's Rust 1.95 is the final choice.

### T02: runtime and FFI

Evaluate a maintained binding against the required libVLC 3 subset. If it cannot provide audited ownership and platform loading with acceptable scope, use a small documented FFI boundary. Do not wrap the entire VLC API. Keep instance/media/player/list/string ownership and release ordering explicit. Reject unsupported ABI/architecture or missing required symbols before use; keep the library handle alive through every wrapper's lifetime. Development runtime lookup and release bundles share one tested policy.

Acceptance: compatible runtime opens; missing DLL/shared library, wrong major, wrong architecture, missing symbol and missing plugin cases produce useful errors; repeated create/release does not leak/crash. Check against actual selected headers. Read runtime version from the loaded library; do not confuse Android VLC 3.7.1 with desktop libVLC 3.

### T03: rendering spike

Prototype libVLC video callbacks into owned CPU frames consumed on Slint's UI thread. Leave sound on VLC's native audio output. Verify callback alignment/pitch/format, aspect ratio, resizing, color order, pause/seek, overlays, and teardown. Compare native output only when needed to explain measured limitations. Document any X11 initialization order required by a native route.

Acceptance on Arch Wayland **and** X11: local H.264/AAC 1080p30 plays for ten minutes; controls remain responsive; audio is synchronized; the frame mailbox is bounded; record CPU, resident memory, dropped/rendered frames and hardware/runtime details. Repeat open/stop/reopen/close at least 20 times. Target less than 1% presentation drops after warm-up and no sustained memory growth; if measurements cannot support this, keep M0 open. Early Windows/macOS smoke runs must prove the same mechanism can display frames or record a target-specific blocker.

M0 does not promise 4K60, HDR, zero-copy, or hardware decode. If the CPU route fails, the Sol reviewer compares a native surface/GPU route or a stack revision using evidence; the parent updates the plan before library UI expands. Do not silently downgrade Wayland support to XWayland.

### T04: integration contract freeze

Parent publishes the actual commands/events, bounded-channel policy, storage methods, error types and minimal examples for workers. Include load generation, non-lossy paths, scan outcomes and shutdown acknowledgement. Assign exclusive files and CI commands per task. Accept only after the Sol reviewer checks T02/T03 safety and the shared contracts; then start the three lanes below.

## M1: working Arch player

T05 implements explicit playback state, load/play/pause/stop, seek, volume, rate/error reporting and clean shutdown. Verify real MP4/MKV/MP3, corrupt/unsupported input, unavailable files, rapid replacement, seeking while loading and closing while decoding. Store no player pointers outside the playback owner.

T07 translates the selected Archive HTML composition to Slint, including a prominent Movies/Music switch at the top of the main section (no proposal-navigation side column), the library shell, title detail/series path, player surface and state/error panels. Music uses poster art for albums, artists and genres, cover thumbnails on track rows, a Playlists section, and a mini-player that remains accessible while browsing either library. Clicking a song row starts that song immediately. Every track row begins with an accessible selection checkbox; the list-level checkbox selects or clears every row in the current list, and Play selected queues checked tracks in displayed order and starts the first. Checkbox changes alone never start playback. Selected tracks can be added in displayed order to a new playlist or a chosen existing playlist. Wire audio views when T13/T14 backends land; do not leave them decorative. Use real records as soon as the backend exists; keep synthetic UI fixtures confined to tests. Preserve focus, selected item, scroll and current playback during artwork/scan updates. First-run, no runtime, empty library, missing artwork, offline root and playback error need usable actions. Check pointer and keyboard access, 100/150/200% scaling, minimum window size and dark/light/system preference.

T09 implements tested queue loading, duplicates, insert-next, append, previous/next, remove/reorder, repeat-off/one/all and shuffle history. Play selected builds the queue from checked entries in the current list's displayed order; clicking one song starts it immediately. Cover empty/single/multiple queues, terminal event deduplication, error stop behavior and manual Next under repeat-one. Playback order follows the active collection rather than the currently visible filtered shelf by accident.

M1 acceptance: launch, open a real local file, play/pause/seek, queue two files, Next/Previous, stop, and close on Arch. The selected title shown in the UI must match the audible/visible media. All controls report actual playback state.

## M2: movie and series library

T06 adds transactional migrations, stable IDs, root membership, progress, playlists, queue/preferences and artwork match records. Tests create a database, reopen it, migrate an older fixture, roll back a failed migration and preserve an unsupported future schema. Test database locking/full-disk errors without losing the original file. Match the schema scope to real tasks; avoid speculative tables for P tasks.

T08 implements explicit root selection, cancellable bounded scans, local browse, list/grid views, search and sort. Test overlapping roots, repeated scans, symlinks, case-sensitive names, unreadable subfolders, cancellation, removable drives and replaced files. Native filenames must round-trip on each OS. Only a complete traversal reconciles missing entries, and no operation deletes media. Large scans must yield progress while the UI stays responsive.

T10 adds movie/episode classification, numeric season/episode ordering, specials, series detail and next available episode. Port behavior through independent tests for source cases, numeric titles (1917/2001), sequel numbers, year/quality suffixes, S01E02, 1x02, ambiguous names and false positives. Keep unknown/multi-episode cases visible as files until correctly supported. Do not invent unavailable episodes or provider metadata.

T11 adds user-enabled TMDB queries, posters **and landscape backdrops** for the selected browsing design, conservative auto-match and manual correction/clear. Test wrong year/sequel, tied candidates, stale jobs, expired/missing credentials, 401/404/429/5xx, timeouts, oversized images and crash-safe cache writes with recorded local responses. Integration checks use a user-provided token without printing it. Cached artwork remains available offline and cache clearing never removes media/history.

M2 acceptance: select a fixture directory with movies and two series, scan, browse/search, open series/season/episode, correct an artwork match, restart and revisit the same records, disconnect a root, and verify history remains. Network disabled and no token must still produce a usable local library.

## M3: music and everyday use

T12 persists resume/history and queue state using the thresholds and event rules in ARCHITECTURE.md. Test pause/stop/close, completion, unknown durations, a manual seek near the end, explicit history clear, and stale progress arriving after the next item loads. Restore paused; never start playback on launch without intent.

T13 adds artists/albums/tracks/genres, disc/track sort, Now playing and embedded/folder artwork. Show album, artist and genre posters plus a track cover on each track row, using real matched local artwork when available and an honest fallback when missing. Unknown metadata remains searchable. Test Unicode tags, missing tags, filename fallback, multi-disc albums, conflicting album artists, corrupt embedded art, and local artwork precedence. Clicking a track starts it immediately. Each track row has a leading accessible checkbox; Select all affects the current displayed list, and Play selected starts the checked tracks in list order. Test empty selection and switching lists while playback continues. Real MP3 and FLAC playback, previous/next-track advancement, play/pause/seek/volume, queue ordering and continuous playback while switching Movies/Music are required. Audio and video use the same serialized playback owner; switching views alone never replaces the current queue.

T14 adds create/rename/edit/delete playlists, add/remove/reorder entries including duplicates, shuffle stored playlist order, queue from playlist, and restart persistence. From the selected track list, add checked songs in displayed order either to a playlist chosen from a listbox or to a newly named playlist. Shuffling must preserve every entry and update the displayed order; use transactions for persistent ordering and test missing/offline entries. Renaming a playlist or display title never renames source files. Playlist import/export can follow v1; do not add it without a task.

T15 adds audio/subtitle track selection, external local subtitle files, subtitle/audio delay, chapters, time jump, speed and aspect/scaling controls. Missing tracks disable the action with an explanation. Test signed track IDs/disabled subtitles, no chapters, malformed subtitles, unsupported speed, and real subtitle rendering. Multi-platform tools must work on the selected render path.

T16 preserves online music-art scope as an explicit optional stage: check current MusicBrainz/CAA and other provider terms/rate limits; start with one documented provider and measured conservative matching. Live-set/YouTube fallback is a separate decision because source markup and terms are a maintenance dependency. Do not silently reproduce scraping from the Android app. This task can remain an explicitly deferred optional feature without blocking local audio release.

T17 implements persisted theme/view/sort/roots/artwork settings, masked token entry via OS credential storage, retry/cache/history actions, About/Credits and redacted diagnostics. Each visible toggle has an actual behavior. Logs exclude tokens, authorization headers and full media paths by default. Verify contrast, accessible names, focus, reduced motion and error recovery. Do not translate every Android preference verbatim.

M3 acceptance: play real audio, navigate artist/album views, create and revisit a playlist, restart/resume a video, exercise tracks/subtitles, switch theme and revisit settings after relaunch. Local functions work offline.

## M4: installation and release

T18 defines an Arch PKGBUILD, desktop entry/MIME association and icons with documented rights. Inspect the current split package dependencies: merely installing a top-level `vlc` package does not prove all playback plugins are present. Test clean install, first launch, decoding fixtures, upgrade with existing data and uninstall preserving user library files. AUR publication is a separate authorized action.

T19 defines an x86_64 Windows package and deterministic runtime/plugin layout, safe DLL discovery, installer upgrade/uninstall behavior and unsigned-development warnings. Test without VLC on PATH or a developer environment. A signed release requires external signing credentials; an unsigned smoke test is not signing evidence.

T20 defines an arm64 macOS `.app`/distribution image with correct runtime/plugin paths and loader install names, entitlements only where needed, and a minimum OS target established in T01. Test from a clean account outside the checkout. Signing/notarization requires user-controlled Apple credentials; record the blocker if absent. Add Intel builds only with their own runtime/test evidence.

T21 runs the native platform matrix, dependency and license inventory, packaged runtime hashes, upgrade/data retention tests and privacy checks. Record package version, source commit, Rust/Slint/libVLC versions, CPU architecture and exact fixture hashes. Produce checksums and a draft release checklist. Publishing, signing-account changes and deployment require task authorization; planning is not permission to release binaries.

## Follow-on parity backlog

| ID | Scope | Entry condition / acceptance |
| --- | --- | --- |
| P01 | Equalizer, bookmarks, A-B repeat, sleep timer | M3 stable; per-feature state/edge-case tests and real playback proof |
| P02 | Network URLs/shares/history, casting | Credential handling, protocol/terms review, bounded connection behavior; tests on real owned server/device |
| P03 | Remote access/pairing, parental controls | Reviewed threat model and explicit product requirements; authentication/access-boundary tests |
| P04 | PiP, media keys/notifications, OS integration | Per-OS capability plan and native tests; no Android-only behavior copied blindly |

Cloud accounts, DRM streaming, transcoding, automatic updating, analytics, and a plugin marketplace are outside the initial scope. Full Android parity remains distinct from the desktop release gates.
