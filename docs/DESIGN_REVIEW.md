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
- Final Archive self-check passed 19 assertions against the actual helper code, including transport state preservation. Sol independently confirmed these assertions. Track rows are list items and the results panel is labelled by the selected tab.
- Archive Music was visually inspected at 1440, 1024 and 800 pixels wide. All six album image ratios measured 1:1; no body horizontal overflow, failed image load or browser warning/error was observed. At 800 pixels, scrolling to the final row leaves the last album action above the fixed mini-player. Queue-boundary keyboard focus moves from the newly disabled Next button to the visible Play/Pause control.
- Source parity inventory: 109 declared, actual and unique Android screen IDs; every non-null task reference resolves to the implementation plan. Relative Markdown links resolve in the workspace.

Screenshots are local, ignored review artifacts under `artifacts/design-review/`. They are not application assets or runtime evidence.

## Independent review

GPT-6.1 Sol at xhigh reviewed a fixed staged snapshot. The initial findings were a failing Archive synopsis expectation, lost keyboard focus in Spotlight/Cinema, a hidden narrow-layout focus fallback in Archive, and a release dependency that accidentally made optional music artwork mandatory. Those defects were corrected. A second stale Archive genre-count expectation was also corrected during isolated execution.

The final audio review found one transport defect (Next/Previous paused the preview) and two small accessibility issues (track list items and tab-panel semantics). These were corrected and rechecked. GPT-6.1 Sol at xhigh reported no residual blocker in final Archive blob `720ba237b4c3c75fc7278ac54e9afc1ef7eb8da6`; the parent reproduced the fixes in the browser. The documentation review also corrected T13 to depend explicitly on the T09 queue API.

**D00 accepted for design/hand-off:** Archive Movies/Music is selected, its local interactions are checked, and final review has no open blocker. The user has not requested Rust implementation in this planning/design pass.

## Limits

No Rust package exists yet. No real video/audio decoding, scanner, provider API, database migration, native accessibility integration or installer has been tested. The previews use illustrative photography and sample metadata; playback controls are simulated. Native gates and fixture requirements are defined in VALIDATION.md.
