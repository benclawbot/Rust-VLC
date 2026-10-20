# Desktop movie presentation proposals

**Selected direction: Archive.** The user selected Archive on 2026-10-20 and then refined it: remove proposal navigation, put a prominent Movies/Music switch at the top of the main section, and add music posters plus a Playlists view. The target for T07 is the full-width poster catalogue, direct search/filter/sort, title detail dialog, and music views. Cinema and Spotlight remain comparison references.

The Music destination includes poster artwork for album, artist, genre and playlist cards, matching cover thumbnails on track rows, album/artist/track/genre/playlist views, album tracks, multi-track selection, and a preview mini-player. Selected songs can be added in displayed order to a new playlist or one chosen from the playlist list. An opened playlist can shuffle its stored track order. The HTML photographs and metadata are sample content; they demonstrate layout, not detected library art or actual decoding. The native audio slice currently has real local tracks and session-only playlists with order shuffling, while movie browsing, tag/artwork extraction and persistence remain follow-up work.

Reading this as: a desktop movie and series library for people browsing their own media, preserving Première's warm cinema identity with Netflix-inspired access through artwork, shelves and title details. This is product UI rather than a marketing site. Design variance 4/10, motion 2/10, density 5/10; Archive intentionally increases density to 7/10.

## Preserved design language

From the inspected [Première proposal stylesheet](https://github.com/benclawbot/VLC-Premiere/blob/5a919454a41f5c490b90e2016437ff77be859da3/design-proposals/premiere/styles.css):

| Token | Value |
| --- | --- |
| Typeface | Inter, then UI/system sans-serif; preview self-hosts Inter Variable |
| Background | `#141312` |
| Main text | `#e7ddcc` |
| Muted text | `#aaa196` |
| Accent | `#f46b32` |
| Separator | `#3b3631` |
| Panel | `#211e1b` |

Retain restrained corners, readable title hierarchy, real artwork, and orange for selection/playback actions. Use legible dark text on orange buttons where ivory would not meet contrast. No Netflix wordmark, red branding, fake ratings, autoplaying promotional video, or unrelated marketing panels.

## Directions to compare

| Proposal | Main decision it explores | Trade-off |
| --- | --- | --- |
| Cinema | One featured title leads into continue-watching and recently-added shelves | Immersive access; fewer titles visible at once |
| Archive | Full-width movie poster catalogue, prominent Movies/Music switch, and artwork-led music views | Faster scanning of larger collections; less featured artwork |
| Spotlight | A selected title anchors the screen while a collection and episodes stay close | Strong title/series context; more deliberate exploration |

Cards open details; explicit Play/Resume actions show simulated feedback. Search/filter and list changes operate only on the sample library. Episode selection demonstrates how local seasons could work. No action scans a disk, contacts a provider or starts VLC.

The previews reuse six photographs and fictional movie titles from the original visual proposal. This keeps composition comparisons consistent and avoids suggesting that a real user's library has been loaded. Photography/fonts are local. See [asset provenance](assets/README.md). Final media cards will use actual matched posters/backdrops and honest filename fallbacks.

## Selection hand-off

The selected source is the current `design/proposals/archive.html`, including the follow-up changes requested on 2026-10-20. Check the desktop and narrow layouts before translating it into Slint. Do not copy HTML or a browser engine into the Rust runtime. Dark/light/system themes and loading/empty/error states remain production tasks even though this proposal concentrates on the requested dark presentation.
