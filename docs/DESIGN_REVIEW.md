# Design review evidence

Date: 2026-10-20. Scope: local HTML design proposals and the implementation hand-off. The user selected Archive and requested audio support. This report does not establish native Rust/libVLC playback.

## Completed checks

- Cinema, Archive Movies and Spotlight were rendered and visually inspected at requested widths 1440, 1024 and 800 pixels. Requested heights were 1000, 768 and 900; the browser reported 769 instead of 768 in the middle case because of viewport rounding. No body horizontal overflow or failed image loads were observed.
- Archive Movies: synopsis search (`signal`), empty results, genre filtering, title sorting, add-to-list, My list filtering, details, and simulated Play were exercised through browser controls.
- Archive focus regression: remove the open saved title at 500 pixels wide, then Esc. The dialog closes and focus moves to the visible search field.
- Cinema: search, saved-only shelf filtering, simulated Play, removal of the open saved title and Esc were exercised. Focus returns to the visible search field when the original opener is hidden.
- Spotlight: Enter on a collection title updates the selected title while retaining focus on the replacement title button. Season 2 and episode selection produce the correct sample episode feedback. Details closes with Esc and restores focus to More info.
- Local JavaScript syntax compilation and asset/route reference checks passed for the selector and all three proposals. The files declare no external asset URLs. Inter is self-hosted and its original font license is included.
- Isolated Node execution of the actual pure model/filter/self-check code passed: Cinema four assertions, Archive Movies seven assertions, Spotlight four assertions. These checks do not simulate a native browser or decoder.
- Archive Music: the `#music` entry, Albums/Artists/Tracks/Genres, album-to-tracks, artist/title search, empty results, genre navigation/filtering, sorting, queue display, and track selection were exercised. Previous/Next preserve both playing and paused preview state. Changing Movies/Music retains the selected track and visible mini-player.
- The earlier Archive Music self-check passed 19 assertions against the helper code, including transport state preservation. Sol independently confirmed that snapshot. Track rows are list items and the results panel is labelled by the selected tab.
- Archive Music was visually inspected at 1440, 1024 and 800 pixels wide. All six album image ratios measured 1:1; no body horizontal overflow, failed image load or browser warning/error was observed. At 800 pixels, scrolling to the final row leaves the last album action above the fixed mini-player. Queue-boundary keyboard focus moves from the newly disabled Next button to the visible Play/Pause control.
- Archive Playlists: create a playlist from six checked songs, open it, shuffle its stored order, and start a song from the shuffled list. The reordered list retained all six songs, the matching preview song started, and keyboard focus stayed on Shuffle order. The create flow, existing-playlist selection, multi-track add, duplicate rejection, and focus return to Add to playlist were also exercised. At 1887×1096 there was no horizontal overflow, all images loaded, and no browser warnings/errors were recorded.
- Browser regression: start Queue A, open and shuffle equal-content Queue B, and confirm A's active queue remains unchanged; then shuffle A and confirm its queue follows the new order while First Light stays current.
- The latest Archive self-check passed 41 assertions in an isolated Node VM, covering playlist creation/addition, de-duplication, queue ownership, shuffle order, current-track preservation and selection ordering.
- Source parity inventory: 109 declared, actual and unique Android screen IDs; every non-null task reference resolves to the implementation plan. Relative Markdown links resolve in the workspace.

Screenshots are local, ignored review artifacts under `artifacts/design-review/`. The latest playlist view was inspected in the in-app browser; it is not application art or native runtime evidence.

## Independent review

GPT-6.1 Sol at xhigh reviewed a fixed staged snapshot. The initial findings were a failing Archive synopsis expectation, lost keyboard focus in Spotlight/Cinema, a hidden narrow-layout focus fallback in Archive, and a release dependency that accidentally made optional music artwork mandatory. Those defects were corrected. A second stale Archive genre-count expectation was also corrected during isolated execution.

The final audio review found one transport defect (Next/Previous paused the preview) and two small accessibility issues (track list items and tab-panel semantics). These were corrected and rechecked. GPT-6.1 Sol at xhigh reported no residual blocker in final Archive blob `720ba237b4c3c75fc7278ac54e9afc1ef7eb8da6`; the parent reproduced the fixes in the browser. The documentation review also corrected T13 to depend explicitly on the T09 queue API.

A later GPT-6.1 Sol xhigh review found a P2 queue-ownership issue when a different playlist had identical tracks. Queue source IDs now flow through direct track and selected-track playback, and shuffle updates only its owning active queue. The targeted re-review found no blocker and verified 41 preview assertions plus four queue-provenance scenarios; the parent also confirmed both same-playlist and equal-content-other-playlist behavior in the browser.

**D00 accepted for design/hand-off:** Archive Movies/Music is selected, its local interactions are checked, and final review has no open blocker. Rust implementation is underway as a separate milestone; D00 acceptance does not establish native playback or release readiness.

## Limits

An initial Rust/Slint audio slice now exists and Windows build/unit checks pass. An explicit libVLC 3 regression smoke test passed, but the native window was not visually inspected and audible speaker output was not checked. No Arch Wayland/X11 video rendering, macOS/Windows release package, scanner, metadata/artwork extraction, database migration or persistent playlist behavior has been validated. The HTML preview uses illustrative photography and sample metadata; its playback controls are simulated. Native gates and fixture requirements are defined in VALIDATION.md.
