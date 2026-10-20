# Rust-VLC agent instructions

Read README.md, docs/IMPLEMENTATION_PLAN.md, docs/ARCHITECTURE.md, and docs/HANDOFF.md before implementation. This repository currently contains planning and HTML design previews. The user selected Archive on 2026-10-20; design/proposals/archive.html is the visual target. Rust implementation has not started.

## Roles and scope

- Implementation workers use `gpt-6-luna` with reasoning effort `max`.
- Independent reviewers use `gpt-6.1-sol` with reasoning effort `xhigh`.
- If an exact model or effort is unavailable, report the gap; do not silently substitute it.
- Each worker receives one task ID, a base commit, and exclusive paths. The parent owns integration and shared contracts. At most three workers run alongside the parent.
- Prefer one Cargo package with modules. Do not add a webview, web server, plugin architecture, or extra workspace crates to the production app.
- Keep source media read-only. Removing a root or library entry must not delete media files.
- Protect native path round-trip, callback lifetime, bounded queues, scan reconciliation, credentials, and database migrations.
- No libVLC calls from libVLC callbacks. No Slint objects off the UI thread. Unsafe code belongs inside the reviewed VLC boundary.
- Prototype images and sample metadata must never be bundled as a real user's library.
- Preserve unrelated changes. Coordinate shared Cargo/schema/contracts files through the parent. Do not commit, push, publish, merge, or change visibility unless the current task authorizes it.

## Design

Read the user's available design-taste-frontend and anti-slop-design skills for UI work. Preserve Première's verified font stack and palette in design/README.md. Use accessible controls, visible focus, reduced motion, and complete empty/loading/error states. Inspect rendered UI and core interactions at representative sizes. No decorative control may masquerade as a working feature.

## Verification and delivery

New non-trivial logic needs a focused test or assert-based self-check. Follow docs/VALIDATION.md; distinguish headless tests, compilation, and real native playback. Missing hardware or credentials is a recorded gate, never a passing result. Return changed files, exact checks and outcomes, unverified behavior, and remaining risks. Do not mark a task complete before its acceptance criteria pass.
