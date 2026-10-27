# Luna Max implementation hand-off

Current release, 2026-10-27: version 0.1.3 adds TMDB genres and poster selection, relinking, library locking and playback transition fixes. The release workflow verified clean installation and upgrade retention on disposable runners; see [validation](VALIDATION.md) for the boundaries of that evidence. The older task catalogue below is the 0.1.1 baseline. Exact Luna Max workers implemented repairs and Sol xhigh reviewers assessed frozen source. Parental/PIN locking and paired LAN remote control remain outside scope by the user's explicit choice.

The user selected **Archive** on 2026-10-20 and required audio support. The latest visual target is `design/proposals/archive.html`: the proposal sidebar is removed, a prominent Movies/Music switch sits above the collection, and Music has artwork-led views and a Playlists section. Double-clicking a movie starts it in fullscreen; leaving fullscreen keeps playback running. Clicking a song starts it immediately. Each track row starts with a checkbox; Select all acts on displayed rows, Play selected queues checked displayed rows in order, and selected tracks can be added to a new or listbox-selected playlist. An opened playlist can shuffle its stored track order. The HTML controls are a sample preview; production code uses Slint.

## Required roles

| Role | Model | Reasoning effort |
| --- | --- | --- |
| Implementation worker | `gpt-6-luna` | `max` |
| Independent reviewer | `gpt-6.1-sol` | `xhigh` |
| Parent/integrator | Existing coordinating session | Current settings |

If these exact settings are unavailable, report it. Review and implementation are different roles; a worker's self-check does not replace independent Sol review. Spawn model-overridden agents with fresh context and include the exact task, files, base commit, selected design and relevant contracts. Do not create user-owned sidebar chats for internal subtasks.

## Dispatch order and ownership

1. D00 is accepted with the current Archive revisions recorded in `design/README.md`.
2. The native movie/music implementation is in the working tree. Read the actual architecture, UX parity review and validation evidence before assigning remaining work.
3. Run T02, then T03 on Arch. Sol reviews FFI lifetime and rendering measurements. A Windows audio smoke test does not close M0; require the Wayland and X11 rendering evidence in the plan.
4. Parent completes T04 contract publication and assigns three non-overlapping lanes: T05 playback, T06 storage, T07 UI.
5. Integrate reviewed lanes. Run T08 scanner, T09 queue, then T10 series. Start T11 artwork and T12 history only after their storage APIs exist.
6. Run T13 audio and T14 playlists in separate modules while T15 tools uses exclusively assigned playback/UI components. Parent/T07 owner wires the resulting views.
7. T16 optional online music is separate from local audio. Run T17 settings with coordinated UI ownership, then packaging T18/T19/T20 and T21.

Use at most three workers plus parent. A Sol review occupies one worker slot, so reduce implementation concurrency while reviewing. Do not have a reviewer assess code still being edited.

For same-checkout work, exclusive files are mandatory. The parent owns Cargo files, module exports, model.rs, central schema and final wiring unless a task explicitly transfers them. For parallel changes that need overlapping files, use isolated managed worktrees and integrate reviewed commits sequentially. Begin from a recorded base commit; preserve unrelated local work. Never cherry-pick or merge a worker's unreviewed partial result.

## Worker prompt template

Replace every angle-bracket field before dispatch. The parent provides real contract signatures and a real base commit, not just this document.

```text
Work as an implementation subagent using gpt-6-luna with max reasoning.

Repository: <absolute Rust-VLC checkout>
Base commit: <verified SHA>
Task: <one ID from docs/IMPLEMENTATION_PLAN.md and its acceptance text>
Selected visual reference: <proposal file, selected revision, user changes>
Dependencies already merged: <task IDs and SHAs>
Exclusive writable paths: <exact files/directories>
Shared files owned by parent: <Cargo/schema/model/app/module-export paths>
Contracts: <actual signatures, event ordering, errors, invariants, examples>

Read AGENTS.md and the relevant plan/architecture/validation sections.
Inspect callers, nearby patterns and existing tests before changing code.
Implement the smallest complete change for this task. Do not implement future tasks.
Do not edit shared/other workers' files; send the parent a concrete proposed change.
Protect media files, native paths, offline use, callback lifetime, bounded queues,
credentials, migration integrity and scan reconciliation.
Run <specific required commands> and <native smoke test where required>.
Do not mark an unavailable native test as passed. Do not commit/push unless assigned.

Return: changed files; concrete behavior; exact test commands/results; any gate
still open; and a short note of skipped checks/risks. Stop after this task.
```

## Sol reviewer prompt template

```text
Review independently using gpt-6.1-sol with xhigh reasoning.
Repository/worktree: <absolute path>
Base/head: <verified SHAs or a fixed named diff>
Task and acceptance criteria: <IDs and exact criteria>
Selected proposal/contracts: <paths and versions>
Evidence supplied: <test outputs, native report, screenshots, measurements>

Read connected code and callers, not only the diff. Review correctness, ownership,
FFI ABI/lifetime/teardown, concurrency/backpressure, native paths, data retention,
migrations, offline behavior, credentials/network boundaries and UI accessibility
where relevant. Challenge unsupported playback or cross-platform claims.
Check task completion against evidence. Do not implement features or modify files.
Return findings ordered by severity with exact files/lines and a reproducible
failure or reason. Separate blockers from optional follow-up. If there are no
findings, say so and list missing verification. Review only the fixed revision.
```

## Current native implementation

The application imports local audio/video files and folders, reads tags/artwork, persists its library/playlists/progress, groups movies into series/seasons and music into albums/artists/genres, and runs both through one libVLC owner. Displayed-row selection, duplicate playlist occurrences, stored playlist shuffle, playback queue editing, fullscreen and core subtitle/audio tools are wired into Slint. TMDB's masked Read Access Token is saved from Options through the system credential store. Music artwork is matched automatically from public YouTube results, then may fall back to MusicBrainz/Cover Art Archive; users do not paste per-track links.

The implementation uses flat modules rather than the speculative subdirectories in the task catalogue. Use [ARCHITECTURE.md](ARCHITECTURE.md) for current ownership and APIs. Version 0.1.2 implements persistent roots/reconciliation, queue restoration and the approved P01/P02/P04 controls; native acceptance remains in validation. P03 is explicitly excluded. The HTML preview defines composition; it does not prove native playback or performance. Windows smoke evidence does not close the Arch Wayland/X11 or macOS gates.

## Integration and completion

Parent checks each worker's files and test evidence, obtains Sol review, resolves findings through a Luna worker, reruns affected checks and updates task status. A milestone is accepted only after its integrated user path passes. Persist an evidence report under artifacts/ locally and commit a redacted summary with runtime/OS/architecture/source SHA when appropriate. Exclude media fixtures, credentials and machine-specific private paths.

Before an authorized push, review staged files for secrets, generated runtime libraries, user media and accidental sample-library inclusion. Release publication and external messaging are separate actions from implementation. Record blockers precisely rather than marking gates complete.
