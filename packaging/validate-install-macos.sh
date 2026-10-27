#!/usr/bin/env bash
set -euo pipefail

version="${GITHUB_REF_NAME#v}"
[[ -n "$version" ]]
[[ "$(uname -m)" == "$EXPECTED_ARCH" ]]
current_name="rust-vlc-macos-${EXPECTED_ARCH}.pkg"
old_name="$current_name"
current_pkg="$GITHUB_WORKSPACE/dist/$current_name"
temp_root="$(mktemp -d "$RUNNER_TEMP/rust-vlc-install-validation.XXXXXX")"
old_dir="$temp_root/old-release"
mkdir -p "$old_dir"
app="/Applications/Rust-VLC.app"
receipt="org.rustvlc.desktop"
data_dir="$HOME/Library/Application Support/org.benclawbot.Rust-VLC"
database_script="$GITHUB_WORKSPACE/packaging/validate-install-database.py"
database_path="$data_dir/library.sqlite3"

check_checksum() {
  local file="$1" sidecar="$2" name expected actual
  name="$(basename "$file")"
  read -r expected sidecar_name < "$sidecar"
  [[ "$sidecar_name" == "$name" && "$expected" =~ ^[[:xdigit:]]{64}$ ]]
  actual="$(shasum -a 256 "$file" | awk '{print $1}')"
  actual="$(printf '%s' "$actual" | tr '[:upper:]' '[:lower:]')"
  expected="$(printf '%s' "$expected" | tr '[:upper:]' '[:lower:]')"
  [[ "$actual" == "$expected" ]]
}

gh release download v0.1.2 --repo "$GITHUB_REPOSITORY" \
  --pattern "$old_name" --pattern "$old_name.sha256" --dir "$old_dir"
check_checksum "$old_dir/$old_name" "$old_dir/$old_name.sha256"
check_checksum "$current_pkg" "$current_pkg.sha256"

assert_not_installed() {
  [[ ! -e "$app" ]]
  ! pkgutil --pkg-info "$receipt" >/dev/null 2>&1
}

install_package() {
  sudo installer -pkg "$1" -target /
}

assert_installed() {
  local expected_version="$1" expected_commit="${2:-}" app_version build_info plugin_count target
  app_version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$app/Contents/Info.plist")"
  [[ "$app_version" == "$expected_version" ]]
  pkgutil --pkg-info "$receipt" | grep -Fxq "version: $expected_version"
  test -x "$app/Contents/MacOS/rust-vlc"
  test -s "$app/Contents/Resources/Rust-VLC.icns"
  test -d "$app/Contents/Frameworks/VLC.app/Contents/MacOS/plugins"
  local libvlc_path
  libvlc_path="$(find "$app/Contents/Frameworks/VLC.app" -name libvlc.dylib -print -quit)"
  test -n "$libvlc_path" && test -e "$libvlc_path"
  plugin_count="$(find "$app/Contents/Frameworks/VLC.app/Contents/MacOS/plugins" -type f -name '*.dylib' | wc -l | tr -d ' ')"
  test "$plugin_count" -gt 0
  lipo "$app/Contents/MacOS/rust-vlc" -verify_arch "$EXPECTED_ARCH"

  build_info="$app/Contents/Resources/BUILD-INFO.txt"
  target=x86_64-apple-darwin
  if [[ "$EXPECTED_ARCH" == arm64 ]]; then target=aarch64-apple-darwin; fi
  grep -Fxq "Target: $target" "$build_info"
  grep -Fxq "Version: $expected_version" "$build_info"
  grep -Eq '^Source commit: [0-9a-f]{40}$' "$build_info"
  grep -Fxq 'Bundled VLC runtime: 3.0.24' "$build_info"
  if [[ -n "$expected_commit" ]]; then grep -Fxq "Source commit: $expected_commit" "$build_info"; fi
}

new_sentinel() {
  mkdir -p "$data_dir"
  local path="$data_dir/$1"
  printf 'synthetic Rust-VLC retention sentinel: %s\n' "$1" > "$path"
  printf '%s\t%s\n' "$path" "$(shasum -a 256 "$path" | awk '{print $1}')"
}

assert_sentinel() {
  local path="$1" expected="$2" actual
  test -f "$path"
  actual="$(shasum -a 256 "$path" | awk '{print $1}')"
  [[ "$actual" == "$expected" ]]
}

forget_package_receipt() {
  sudo pkgutil --forget "$receipt"
  assert_not_installed
}

assert_not_installed

# Verify a clean current-version package install and its manual-removal data behavior.
install_package "$current_pkg"
assert_installed "$version" "$GITHUB_SHA"
IFS=$'\t' read -r clean_sentinel clean_hash < <(new_sentinel clean-install-retention.sentinel)
sudo rm -rf "$app"
forget_package_receipt
assert_sentinel "$clean_sentinel" "$clean_hash"

# Install the authenticated published package, upgrade in place, and manually remove the app.
install_package "$old_dir/$old_name"
assert_installed 0.1.2
database_hash="$(python3 "$database_script" create "$database_path")"
IFS=$'\t' read -r upgrade_sentinel upgrade_hash < <(new_sentinel upgrade-retention.sentinel)
install_package "$current_pkg"
assert_installed "$version" "$GITHUB_SHA"
assert_sentinel "$clean_sentinel" "$clean_hash"
assert_sentinel "$upgrade_sentinel" "$upgrade_hash"
python3 "$database_script" verify "$database_path" "$database_hash"
sudo rm -rf "$app"
forget_package_receipt
assert_sentinel "$clean_sentinel" "$clean_hash"
assert_sentinel "$upgrade_sentinel" "$upgrade_hash"
python3 "$database_script" verify "$database_path" "$database_hash"

echo 'macOS clean install, v0.1.2-to-current package upgrade, receipt/version/runtime inventory, synthetic library database/sentinel retention passed. The package has no native uninstaller; only the app bundle was removed. GUI launch and playback were not exercised.'
