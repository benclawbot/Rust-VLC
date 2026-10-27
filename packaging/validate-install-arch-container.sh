#!/usr/bin/env bash
set -euo pipefail

pacman -Syu --noconfirm --needed base-devel libvlc vlc-plugins-all fontconfig libglvnd libx11 libxcb libxcursor libxi libxkbcommon libxkbcommon-x11 wayland python

export HOME=/root
export XDG_DATA_HOME=/root/.local/share
old_name='rust-vlc-0.1.2-1-x86_64.pkg.tar.zst'
new_name="rust-vlc-${CURRENT_VERSION}-1-x86_64.pkg.tar.zst"
data_dir="$XDG_DATA_HOME/rust-vlc"
database_path="$data_dir/library.sqlite3"
database_script=/validate-install-database.py

assert_not_installed() {
  if pacman -Q rust-vlc >/dev/null 2>&1; then echo 'rust-vlc remains registered after removal' >&2; exit 1; fi
  test ! -e /usr/bin/rust-vlc
}

assert_installed() {
  local expected_version="$1" expected_commit="${2:-}" package_info build_info plugin_count
  test -x /usr/bin/rust-vlc
  test -s /usr/share/applications/rust-vlc.desktop
  test -s /usr/share/icons/hicolor/512x512/apps/rust-vlc.png
  test -s /usr/share/doc/rust-vlc/BUILD-INFO.txt
  test -f /usr/lib/libvlc.so.5
  test -d /usr/lib/vlc/plugins
  pacman -Q libvlc vlc-plugins-all >/dev/null
  plugin_count="$(find /usr/lib/vlc/plugins -type f | wc -l | tr -d ' ')"
  test "$plugin_count" -gt 0

  package_info="$(pacman -Q rust-vlc)"
  [[ "$package_info" == "rust-vlc $expected_version-1" ]]
  build_info=/usr/share/doc/rust-vlc/BUILD-INFO.txt
  grep -Fxq 'Target: x86_64-unknown-linux-gnu' "$build_info"
  grep -Fxq "Version: $expected_version" "$build_info"
  grep -Eq '^Source commit: [0-9a-f]{40}$' "$build_info"
  if [[ -n "$expected_commit" ]]; then grep -Fxq "Source commit: $expected_commit" "$build_info"; fi
}

new_sentinel() {
  mkdir -p "$data_dir"
  local path="$data_dir/$1"
  printf 'synthetic Rust-VLC retention sentinel: %s\n' "$1" > "$path"
  sentinel_path="$path"
  sentinel_hash="$(sha256sum "$path" | awk '{print $1}')"
}

assert_sentinel() {
  local path="$1" expected="$2" actual
  test -f "$path"
  actual="$(sha256sum "$path" | awk '{print $1}')"
  [[ "$actual" == "$expected" ]]
}

assert_not_installed

# Verify a clean current-version package install and pacman removal.
pacman -U --noconfirm "/new/$new_name"
assert_installed "$CURRENT_VERSION" "$SOURCE_SHA"
new_sentinel clean-install-retention.sentinel
clean_sentinel="$sentinel_path"
clean_hash="$sentinel_hash"
pacman -R --noconfirm rust-vlc
assert_not_installed
assert_sentinel "$clean_sentinel" "$clean_hash"

# Install the published package, upgrade in place, then remove with pacman.
pacman -U --noconfirm "/old/$old_name"
assert_installed 0.1.2
database_hash="$(python "$database_script" create "$database_path")"
new_sentinel upgrade-retention.sentinel
upgrade_sentinel="$sentinel_path"
upgrade_hash="$sentinel_hash"
pacman -U --noconfirm "/new/$new_name"
assert_installed "$CURRENT_VERSION" "$SOURCE_SHA"
assert_sentinel "$clean_sentinel" "$clean_hash"
assert_sentinel "$upgrade_sentinel" "$upgrade_hash"
python "$database_script" verify "$database_path" "$database_hash"
pacman -R --noconfirm rust-vlc
assert_not_installed
assert_sentinel "$clean_sentinel" "$clean_hash"
assert_sentinel "$upgrade_sentinel" "$upgrade_hash"
python "$database_script" verify "$database_path" "$database_hash"

echo 'Arch clean install, v0.1.2-to-current pacman upgrade/removal, package inventory, VLC dependency/plugin inventory, synthetic library database/sentinel retention passed. GUI launch and playback were not exercised.'
