# Rust-VLC runtime requirements

The Rust-VLC installers do not include VLC or VLC plugins. Install a compatible
VLC 3 runtime with its plugins, or set `VLC_DIR` to the absolute VLC installation
directory. It must contain the libVLC library and the platform's expected
plugin directory (`plugins` on Windows and macOS; commonly `vlc/plugins` beside
Linux system libVLC). The library, plugins, and executable must use the same CPU
architecture. Plugin and codec availability depends on the installed VLC
distribution.

The Linux installer installs into `$HOME/.local` by default. To choose another
prefix, set `RUST_VLC_INSTALL_DIR` before running `install.sh`. Windows Setup
installs the application and shortcuts; macOS packages install `Rust-VLC.app`
under `/Applications`.

- Linux x86_64 was built on Ubuntu 22.04 and requires glibc 2.35 or newer, a
  desktop session using X11 or Wayland, and the matching system windowing and
  graphics libraries. On Arch Linux, the standard VLC library and plugin paths
  are discovered automatically.
- Windows x86_64 statically links the MSVC C runtime and requires a compatible
  installed VLC 3 x64 runtime and plugins; no separate Visual C++ Redistributable
  is needed.
- macOS packages and app bundles are unsigned and not notarized. macOS may warn
  before installation or first launch. Install VLC 3 for the same architecture;
  the app checks `/Applications/VLC.app` and accepts `VLC_DIR`.

VLC files and signing credentials are not included. User library data remains in
the operating system's application-data directory and is not removed by the
installer or uninstaller.
