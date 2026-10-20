# VLC Première reference audit

Inspected 2026-10-20 at commit [`5a919454a41f5c490b90e2016437ff77be859da3`](https://github.com/benclawbot/VLC-Premiere/commit/5a919454a41f5c490b90e2016437ff77be859da3), default branch `master`. Findings below describe inspected source/documents, not a newly tested Android installation.

## Evidence

- [Root README](https://github.com/benclawbot/VLC-Premiere/blob/5a919454a41f5c490b90e2016437ff77be859da3/README.md) describes a native Android redesign over VLC Android 3.7.1, built by patching/repackaging the retained engine.
- [Engine bridge](https://github.com/benclawbot/VLC-Premiere/blob/5a919454a41f5c490b90e2016437ff77be859da3/android-premiere/ENGINE-BRIDGE.md) uses Android Medialibrary, PlaybackService, MediaUtils, and activity APIs. Those services do not become desktop APIs by recompilation.
- [Design target](https://github.com/benclawbot/VLC-Premiere/blob/5a919454a41f5c490b90e2016437ff77be859da3/android-premiere/DESIGN-TARGET.md) specifies a real library, continue-watching art, shelves, audio tabs, playlists, browsing, and grouped settings.
- [Proposal stylesheet](https://github.com/benclawbot/VLC-Premiere/blob/5a919454a41f5c490b90e2016437ff77be859da3/design-proposals/premiere/styles.css) declares Inter with a system sans-serif fallback. The proposal does not load a font file; an unavailable Inter falls back. Rust-VLC's previews explicitly self-host Inter to make review consistent.
- [Artwork implementation](https://github.com/benclawbot/VLC-Premiere/blob/5a919454a41f5c490b90e2016437ff77be859da3/android-premiere/ARTWORK-IMPLEMENTATION.md) describes title/tag matching, query/entity caches, TMDB, MusicBrainz/CAA, Deezer, and YouTube fallback behavior. Its claimed Android validation is historical evidence only.
- [SeriesUX.java](https://github.com/benclawbot/VLC-Premiere/blob/5a919454a41f5c490b90e2016437ff77be859da3/android-premiere/native/org/videolan/vlc/premiere/SeriesUX.java) isolates filename season/episode detection. [ArtworkMatch.java](https://github.com/benclawbot/VLC-Premiere/blob/5a919454a41f5c490b90e2016437ff77be859da3/android-premiere/native/org/videolan/vlc/premiere/ArtworkMatch.java) contains numeric-title, year-conflict, ambiguity, fuzzy matching, and music-name behavior worth reproducing through independently written tests.
- [Phone library screenshot](https://github.com/benclawbot/VLC-Premiere/blob/5a919454a41f5c490b90e2016437ff77be859da3/android-premiere/previews/phone/video-library.png) was visually inspected: warm dark background, ivory headings, orange controls, a large selected title image and a recently-added shelf.
- [109-screen inventory](https://github.com/benclawbot/VLC-Premiere/blob/5a919454a41f5c490b90e2016437ff77be859da3/design-proposals/premiere/coverage.json) is the Android proposal scope, not a desktop requirement or runtime coverage result. [source-parity.json](source-parity.json) assigns each original screen a desktop disposition.

## Scope disposition

| Behavior | Desktop treatment | Tasks |
| --- | --- | --- |
| Visual navigation and movie presentation | Redesign for desktop, preserve typography/palette; user chooses HTML proposal | D00, T07 |
| Decoding and playback | Reuse desktop libVLC 3 API; do not port Android services | T02, T03, T05 |
| Local library, roots, browse/search | Implement native desktop scan/storage; no automatic whole-disk scan | T06, T08 |
| Queues, previous/next, resume/history | Rust state machine and persistent records | T09, T12 |
| Series and seasons | Independently implement tested filename behavior, add conservative `1x02` support | T10 |
| TMDB posters and backdrops | Optional token, local cache, manual corrections, attribution | T11 |
| Audio/artist/album/genre and artwork | Embedded/folder art first; optional online provider stage separately tracked | T13, T16 |
| Playlists | Native ordered persisted playlists; duplicates supported | T14 |
| Tracks, external subtitles, rate, sync, chapters, scaling | Desktop player tools | T15 |
| Settings, themes, diagnostics, About | Native panels with real values and privacy controls | T17 |
| Equalizer, bookmarks, A-B repeat, sleep timer | Explicit follow-on parity, not implied in initial desktop release | P01 |
| Network shares, URL streams/history, casting | Separate network-capability scope and credentials/terms review | P02 |
| Remote access/pairing and parental PIN | Separate threat model and access controls before implementation | P03 |
| Popup/PiP, OS media keys/notifications, shell integration | Platform follow-on, validated per OS | P04 |
| Android Auto, widgets, Android permission/gesture flows | Android-only; adapt only meaningful desktop access/error states | N/A |

Every P task remains in the backlog. The first desktop release is deliberately smaller than Android's 109-screen proposal, and must not be described as full parity. Source screen counts do not cover desktop-only series detail, artwork correction, runtime setup, or migration errors; add those to the desktop test inventory.

## Provenance and distribution decisions

GitHub reports no detected root project license for VLC-Premiere. That does not establish that every source file or asset has the same status. Do not copy its Java/smali, Android binaries, signing material, or third-party brand marks into the desktop runtime. Treat it as a behavior reference while recording code provenance. The HTML preview photography is taken from the user-supplied reference repository and is documented in design/assets/README.md; confirm its upstream rights before public distribution.

The application license is not chosen in this planning commit. T01 records the intended application license; T21 verifies the actual dependency/runtime/plugin inventory before distributable releases. [Slint lists multiple license options](https://slint.dev/pricing) with [published terms](https://slint.dev/terms-and-conditions); review the chosen version and applicable obligations. Do not assume libVLC's core terms cover every bundled VLC component.

TMDB requires an approved logo and its prescribed attribution in About/Credits, and distinguishes commercial licensing. Use the exact current notice from the [TMDB FAQ](https://developer.themoviedb.org/docs/faq) when implementing; review it again before release. This plan is an engineering checklist, not a completed licensing determination.
