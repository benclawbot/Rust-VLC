# Selected desktop interface

The selected direction is the **Archive style**: a full-width movie catalogue and an artwork-led music library, joined by a prominent **Movies / Music** switch. “Archive” describes the visual style; the movie page itself is titled **Movies**. The single selected preview is [archive.html](proposals/archive.html).

## Movies

The movie page uses poster cards, search, genre filters, sorting, a saved list and title details. Its catalogue density makes a larger local collection easier to scan while retaining Première's warm dark palette and clear orange actions.

## Music

Music uses poster-style cards for albums, artists, genres and playlists, with cover thumbnails on track rows. Users can start a song by clicking its row, select several tracks with leading checkboxes, add the selection to a new or existing playlist, and shuffle an opened playlist's stored order.

## Visual language and limits

The preview preserves Première's Inter/system sans-serif stack and Archive composition. The user's 2026-10-21 palette refinement uses dark brown surfaces (`#141312`, `#211e1b`), white text and orange accents (`#f46b32`), including focus, selection and playback controls. Blue system accents must not enter the application layout. Preserve the prominent Movies/Music switch at the top left, full-width catalogue, portrait movie posters and square music artwork.

It is a local HTML design reference, not the production UI. Sample titles and photographs do not represent a scanned library, and preview playback does not decode media or contact artwork providers. The native app is a separate Rust/Slint implementation.

See [asset provenance](assets/README.md) before redistributing the preview photography or font assets.

## Video player proposal

[video-refinement.html](proposals/video-refinement.html) is a separate playback proposal under review. It uses an edge-to-edge picture, central Previous / Play-Pause / Next controls, a plain timeline and a Tools drawer for audio, subtitles and speed. The duplicate bottom transport, chapter markers/list, Theatre toggle and top proposal label have been removed following review. Its playback remains simulated; it does not replace Archive as the Movies and Music library reference.
