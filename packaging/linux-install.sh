#!/usr/bin/env bash
set -euo pipefail

package_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
prefix="${RUST_VLC_INSTALL_DIR:-$HOME/.local}"
binary="$prefix/bin/rust-vlc"
desktop_file="$HOME/.local/share/applications/rust-vlc.desktop"
desktop_exec="$(printf '%s' "$binary" | sed 's/[\\"]/\\&/g')"

install -D -m 0755 "$package_dir/rust-vlc" "$binary"
install -D -m 0644 "$package_dir/README-runtime.md" "$prefix/share/doc/rust-vlc/README-runtime.md"
install -D -m 0644 "$package_dir/Inter-OFL.txt" "$prefix/share/licenses/rust-vlc/Inter-OFL.txt"
install -D -m 0644 /dev/stdin "$desktop_file" <<EOF
[Desktop Entry]
Type=Application
Name=Rust-VLC
Comment=Local movie and music player
Exec="$desktop_exec"
Terminal=false
Categories=AudioVideo;Player;Video;Audio;
EOF

printf 'Installed Rust-VLC to %s\n' "$binary"
printf 'Install a matching VLC 3 runtime and plugins; see %s\n' "$prefix/share/doc/rust-vlc/README-runtime.md"
