# Rust-VLC runtime requirements

Release installers provide the VLC 3 runtime needed for playback. Windows and
macOS bundle VideoLAN VLC 3.0.24 with its plugins for the matching CPU
architecture. The Arch Linux package depends on the official `libvlc` and
`vlc-plugins-all` packages, so `pacman` installs the runtime and full plugin set
automatically without installing the VLC desktop application.

The Windows installer installs the app and runtime together and creates a
desktop shortcut by default. The macOS package installs `Rust-VLC.app` in
`/Applications`, with VLC nested inside the app bundle. The Arch package adds
Rust-VLC to the application launcher with its icon.

Windows x86_64 statically links the MSVC C runtime, so a separate Visual C++
Redistributable is not required. macOS packages are unsigned and not notarized;
macOS may warn before installation or first launch. Linux packages target Arch
Linux x86_64 and use the distribution's native libraries.

When running a binary built from source, install a matching VLC 3 runtime and
plugins yourself or set `VLC_DIR` to the absolute VLC installation directory.
Keep the library, plugins, and executable on the same CPU architecture. See
[VLC third-party notices](VLC-THIRD-PARTY.md) for bundled versions, licenses,
and upstream source links.

User library data remains in the operating system's application-data directory
and is not removed by the installer or uninstaller.
