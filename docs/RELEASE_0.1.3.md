# Rust-VLC 0.1.3

- Automatically fill missing movie and TV genres from TMDB when a read token is configured, and choose a poster from search results. Existing genres are retained; ambiguous or incomplete searches are not applied.
- Relink a local media entry to a moved file while retaining its library identity, playlists, progress and bookmarks.
- Prevent current-version app instances from opening the same library simultaneously. Close older versions before opening or upgrading a shared library, because they do not participate in this lock.
- Keep music artwork lookup moving past network tracks, and update fullscreen presentation when a queue switches between video and audio.
- Gate release packages on clean-install and 0.1.2 upgrade checks with synthetic library data on disposable runners. These checks cover package installation and data retention, not native GUI or playback.

See the [README](../README.md) for installation, artwork privacy and usage. See [validation](VALIDATION.md) for test evidence and remaining native-platform checks.
