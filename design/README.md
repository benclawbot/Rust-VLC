# Desktop movie presentation proposals

**Selected direction: Archive.** The user selected the Archive view on 2026-10-20 after browsing the HTML proposals. Its poster catalogue, compact sidebar, search/filter/sort and title detail dialog are the visual target for T07. Cinema and Spotlight remain comparison references. No Rust implementation has started.

The user also required audio support. Archive includes a Music destination with square album artwork, artists/albums/tracks/genres, album tracks, a queue and a persistent preview mini-player. Movie presentation stays poster-led; music uses the same typography, spacing and navigation with formats suited to audio. These are sample interactions, not actual decoding. The native application must validate both media types before release.

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
| Archive | Movie poster catalogue and a music library sharing direct search and a compact sidebar | Faster scanning of larger collections; less featured artwork |
| Spotlight | A selected title anchors the screen while a collection and episodes stay close | Strong title/series context; more deliberate exploration |

Cards open details; explicit Play/Resume actions show simulated feedback. Search/filter and list changes operate only on the sample library. Episode selection demonstrates how local seasons could work. No action scans a disk, contacts a provider or starts VLC.

The previews reuse six photographs and fictional movie titles from the original visual proposal. This keeps composition comparisons consistent and avoids suggesting that a real user's library has been loaded. Photography/fonts are local. See [asset provenance](assets/README.md). Final media cards will use actual matched posters/backdrops and honest filename fallbacks.

## Selection hand-off

The selected source is `design/proposals/archive.html` from the initial repository commit containing this document. No combination with another direction or additional visual change has been requested. Capture its desktop and narrow layout and freeze a small Slint token/component contract before T07. Do not copy HTML or a browser engine into the Rust runtime. Dark/light/system themes and loading/empty/error states remain production tasks even though these proposals concentrate on the requested dark movie presentation.
