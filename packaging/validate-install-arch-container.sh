#!/usr/bin/env bash
set -euo pipefail

# The official Arch container excludes documentation with a pacman NoExtract
# rule. Whitelist this package's documentation in the disposable container so
# pacman installs and validates the same files that the package archives.
if pacman-conf NoExtract | grep -Fq 'usr/share/doc/*'; then
  if ! grep -Eq '^[[:space:]]*NoExtract[[:space:]]*=.*usr/share/doc/\*' /etc/pacman.conf; then
    echo 'Arch container excludes documentation through an unexpected pacman config include' >&2
    exit 1
  fi
  sed -i -E '/^[[:space:]]*NoExtract[[:space:]]*=.*usr\/share\/doc\/\*/a NoExtract = !usr/share/doc/rust-vlc/*' /etc/pacman.conf
  pacman-conf NoExtract | grep -Fq '!usr/share/doc/rust-vlc/*' || {
    echo 'Failed to whitelist Rust-VLC package documentation in pacman config' >&2
    exit 1
  }
  echo 'Whitelisted Rust-VLC documentation against the Arch container NoExtract filter.'
fi

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

fail_check() {
  echo "FAILED install check: $*" >&2
  exit 1
}

require_file() {
  local description="$1" path="$2" kind="${3:-nonempty}"
  case "$kind" in
    nonempty) [[ -s "$path" ]] || fail_check "$description is missing or empty: $path" ;;
    present) [[ -f "$path" ]] || fail_check "$description is missing: $path" ;;
    executable) [[ -x "$path" ]] || fail_check "$description is missing or not executable: $path" ;;
    *) fail_check "unknown file check kind '$kind' for $description" ;;
  esac
}

require_directory() {
  local description="$1" path="$2"
  [[ -d "$path" ]] || fail_check "$description is missing: $path"
}

require_build_info_line() {
  local expected="$1" file="$2"
  grep -Fxq -- "$expected" "$file" || fail_check "build info lacks '$expected': $file"
}

assert_installed() {
  local expected_version="$1" expected_commit="${2:-}" package_info build_info plugin_count
  require_file 'Rust-VLC executable' /usr/bin/rust-vlc executable
  require_file 'desktop entry' /usr/share/applications/rust-vlc.desktop
  require_file '512 px application icon' /usr/share/icons/hicolor/512x512/apps/rust-vlc.png
  require_file 'build info' /usr/share/doc/rust-vlc/BUILD-INFO.txt
  require_file 'libVLC ABI-5 library' /usr/lib/libvlc.so.5 present
  require_directory 'VLC plugin directory' /usr/lib/vlc/plugins
  pacman -Q libvlc vlc-plugins-all >/dev/null || fail_check 'libvlc and vlc-plugins-all are not both installed'
  plugin_count="$(find /usr/lib/vlc/plugins -type f | wc -l | tr -d ' ')"
  [[ "$plugin_count" =~ ^[0-9]+$ ]] && (( plugin_count > 0 )) || fail_check "VLC plugin directory has no regular files: /usr/lib/vlc/plugins"

  package_info="$(pacman -Q rust-vlc)"
  [[ "$package_info" == "rust-vlc $expected_version-1" ]] || fail_check "expected package 'rust-vlc $expected_version-1', got '$package_info'"
  build_info=/usr/share/doc/rust-vlc/BUILD-INFO.txt
  require_build_info_line 'Target: x86_64-unknown-linux-gnu' "$build_info"
  require_build_info_line "Version: $expected_version" "$build_info"
  grep -Eq '^Source commit: [0-9a-f]{40}$' "$build_info" || fail_check "build info has an invalid source commit: $build_info"
  if [[ -n "$expected_commit" ]]; then require_build_info_line "Source commit: $expected_commit" "$build_info"; fi
  echo "Validated Rust-VLC $expected_version package, build metadata, libVLC 3 runtime and $plugin_count VLC plugin files."
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
