# VLC Première experience comparison

Review date: 2026-10-21. Reference: VLC-Première source commit [`5a919454a41f5c490b90e2016437ff77be859da3`](https://github.com/benclawbot/VLC-Premiere/tree/5a919454a41f5c490b90e2016437ff77be859da3). This comparison uses its Java implementation and captured Android screens, rather than a newly installed APK. The desktop target is the user's selected Archive design with the requested Movies/Music, selection and playlist changes.

Implementation workers use GPT-6 Luna with Max effort; independent code review uses GPT-6.1 Sol with xhigh effort. Test and native UI evidence are recorded separately in [VALIDATION.md](VALIDATION.md). The original [109-screen inventory](source-parity.json) assigns planning dispositions; it is not a list of 109 working desktop screens.

## Everyday flows

| Première experience | Desktop implementation | Limit or qualification |
| --- | --- | --- |
| Movie catalogue with artwork and watch status | Movies catalogue, local posters, search, all/unwatched/in-progress/completed filters, saved My List and sort options | Genre choices only work when a movie already has stored genre data; TMDB poster lookup does not yet populate it. Landscape backdrops remain deferred |
| Series and seasons | Conservative S01E02/1x02 recognition, numeric seasons and episodes, queue from chosen episode | Unsupported multi-episode/date patterns remain ordinary files; no invented episodes |
| Continue watching | Persisted movie position, Resume and Start over, completion filter | Desktop uses a documented ten-second resume threshold; Android's in-progress threshold differs |
| Enter and leave video playback | Double-click movie to play fullscreen; Escape leaves fullscreen with playback continuing | Desktop window controls replace Android orientation and gesture controls |
| Keyboard transport | Space toggles play/pause, Left/Right seek ±10 seconds, Ctrl+Left/Right changes media for both video and audio | Text inputs and modal controls retain their own editing keys |
| Audio Artists / Albums / Tracks / Genres | All four views, actual tags, album-artist grouping and cover thumbnails | Artist/genre artwork derives from member tracks; dedicated artist photography is not fetched |
| Activate a song within a collection | Queue the displayed collection at its clicked occurrence | Duplicate occurrences must remain separately addressable |
| Multi-select and play | Leading checkboxes, Select all, Play selected in displayed order | Selection alone never changes playback |
| Persistent player while browsing | One playback owner and mini-player across Movies/Music | No Android service, notification or widget integration |
| Playlists and duplicate confirmation | Create/rename/delete, choose destination, add selection, explicit duplicate option, remove/reorder entries | Import/export remains deferred |
| Playlist shuffle | Shuffle stored entries and preserve duplicates; update its owning active queue without restarting the current song | Unrelated queues and selected subsets must remain unchanged |
| Queue editing | Jump, move, remove and Play next with revision/occurrence guards | Queue state is not restored after application restart |
| Repeat and playback shuffle | Off / All / One, manual Next escapes repeat-one, independent playback shuffle | Saved playlist order and playback traversal remain distinct |
| Local file and folder access | Native file dialogs and bounded additive folder import | Persistent roots, scan reconciliation, relink and a full folder browser remain open |
| Recovery and missing media | Visible runtime/import/playback/storage errors; unavailable entries retained | Installing/fixing a missing VLC runtime can require restarting the app |

The collection/playlist comparison follows `Premiere.java` collection playback, playlist editing, queue and watch-filter paths. The review specifically checks for a singleton queue on song click, global-library selection leaking into a filtered collection, duplicate rows playing the wrong occurrence, and stored shuffle replacing an unrelated queue.

## Playback options

| Android option | Desktop disposition |
| --- | --- |
| Play/pause, stop, previous/next, seeking, volume | Implemented through the serialized libVLC owner |
| Speed | Implemented with validated rate bounds and actual snapshot feedback |
| Audio track selection | Available named audio tracks exposed in player tools |
| Subtitle selection / disable / local file | Available subtitle tracks and Off, plus native local subtitle-file selection |
| Audio and subtitle synchronization | Independent signed delays and reset controls |
| Jump to time and chapters | Validated time input and available chapter list; unavailable operations are disabled |
| Video size and aspect | Fit / fill preserves source aspect; explicit fixed aspect overrides remain deferred |
| Source information | Current file information and playback state; detailed codec/statistics inspector remains deferred |
| Equalizer, bookmarks, A–B repeat, sleep timer, frame screenshot | Desktop-relevant follow-on work P01; absent from this build |
| Network streams/shares/history, subtitle search/download, casting | Follow-on P02; no controls implying an active implementation |
| Remote pairing and parental controls | Follow-on P03, subject to access/security requirements |
| Picture-in-picture, media keys, OS notifications | Follow-on P04 with per-platform qualification |
| Brightness swipes, rotation lock, Android storage permissions, Android Auto | Android-specific; native desktop controls and file dialogs replace applicable access flows |

These options were compared with `VideoUI.java`, `PlaybackUX.java` and the captured audio-player/queue screens. Merely linking libVLC does not mean every VLC option is exposed or tested.

## Artwork and settings

Local embedded and folder art are read first. Movie and TV poster lookup uses a masked token saved through Options in the system credential store; `TMDB_READ_TOKEN` remains an environment fallback. For music without local art, automatic background lookup uses tagged title and artist and accepts only a strong YouTube match. MusicBrainz/Cover Art Archive can provide fallback art and may also receive the album tag. The bounded public search-page parser does not run JavaScript or use login cookies, so a YouTube format change can stop automatic matches until the parser is updated. No file paths are sent. Dedicated artist photography and landscape backdrops remain deferred.

Options contains a masked TMDB Read Access Token field and reports when the OS credential store is unavailable. The secret is not copied into SQLite. Provider switches, history/cache management and persistent library roots remain open. About identifies providers and attribution. The selected dark Archive palette is the only current native theme.

## Findings addressed during review

The fixed source lots were independently reviewed before integration. Review-driven changes include migration backups, native-path retention, compilation album grouping, duplicate selection, queue edge cases, strict artwork matching and redirects, and scan cancellation. Playback fixes publish frames at libVLC's presentation callback, reject stale seeks across duplicate occurrences, clear stale Opening messages, replay ended media and bind resume to the exact new load. Native review also verified the Windows canonical-path adapter with real decoded frames.

UI refresh now preserves model identity, allowing reliable double-click and retained focus. The native palette uses orange, dark brown and white throughout custom controls. Dropdown selection/scrolling, long-text caret movement, slider cancellation and checkbox focus were rechecked in source. Text fields retain native keyboard editing; their standard right-click edit menus remain unimplemented. Integrated command and native interaction results are recorded in validation.

The remaining user-visible parity gaps include equalizer controls, bookmarks and A–B repeat, a sleep timer, frame capture, subtitle search/download, network shares and streams, casting, remote access, picture-in-picture and OS media keys/notifications. Queue restoration and playlist import/export are absent; persistent library roots and full scan reconciliation also remain open. These items are grouped by impact in the implementation plan. Native platform, performance, packaging and licensing gates remain open in validation.
