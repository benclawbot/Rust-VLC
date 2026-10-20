# Rust-VLC

A planned Rust desktop media player inspired by [VLC Première](https://github.com/benclawbot/VLC-Premiere), with Netflix-style access to a local movie and series library.

**Current state: Archive design selected; implementation hand-off prepared. No Rust player has been implemented.**

## Review the HTML proposals

Open [the proposal selector](design/index.html) or serve this directory locally:

```sh
python -m http.server 8765 --bind 127.0.0.1
```

Then visit `http://127.0.0.1:8765/design/`. The previews are plain HTML, CSS, and JavaScript. Their media library is illustrative; Play displays preview feedback. They do not scan files, play video, or contact artwork providers.

| Proposal | Browsing model |
| --- | --- |
| [Cinema](design/proposals/cinema.html) | Featured film, continue-watching cards, and cinematic shelves |
| [Archive](design/proposals/archive.html) | Movie poster catalogue plus music albums/tracks, search and a mini-player |
| [Spotlight](design/proposals/spotlight.html) | A selected title beside its collection and season/episode access |

All three retain Première's Inter typography, warm charcoal, ivory, and orange. The HTML is a design reference for Slint, not the production UI stack.

The user selected **Archive** on 2026-10-20 and required audio support. Its movie catalogue, music library, sidebar and direct search/filter access are the production visual target. Music controls in this HTML are simulated; native playback is a later verified milestone.

## Implementation hand-off

- [Implementation plan](docs/IMPLEMENTATION_PLAN.md): milestones, dependencies, task ownership, and acceptance criteria.
- [Architecture](docs/ARCHITECTURE.md): module boundaries, data model, playback and worker contracts.
- [Agent hand-off](docs/HANDOFF.md): ready-to-use worker/reviewer instructions and dispatch order.
- [Source audit](docs/SOURCE_AUDIT.md): inspected Android reference, feature disposition, fonts, and provenance limits.
- [Validation](docs/VALIDATION.md): automated checks, real playback, and release gates.
- [Design review evidence](docs/DESIGN_REVIEW.md): actual HTML checks and their limits.
- [Design brief](design/README.md): proposal differences and the selection gate.

Implementation workers: **GPT-6 Luna, Max** (`gpt-6-luna`, `max`). Independent review: **GPT-6.1 Sol, xhigh** (`gpt-6.1-sol`, `xhigh`). Do not substitute models silently.

The proposed stack is one Rust Cargo package, Slint, libVLC 3, and SQLite. Arch Linux is the first playback target; macOS and Windows are explicit later release targets. Rendering feasibility precedes the full library UI. Android's VLC 3.7.1 package version does not specify the desktop libVLC ABI.

No project license has been selected. Before public distribution, settle the application license and review source artwork, Slint, the selected VLC runtime/plugins, and provider terms. See [source audit](docs/SOURCE_AUDIT.md).
