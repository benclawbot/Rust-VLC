#!/usr/bin/env bash
set -euo pipefail

version="${GITHUB_REF_NAME#v}"
[[ -n "$version" ]]
old_name='rust-vlc-0.1.2-1-x86_64.pkg.tar.zst'
current_name="rust-vlc-${version}-1-x86_64.pkg.tar.zst"
current_dir="$GITHUB_WORKSPACE/dist"
temp_root="$(mktemp -d "$RUNNER_TEMP/rust-vlc-arch-install-validation.XXXXXX")"
old_dir="$temp_root/old-release"
mkdir -p "$old_dir"

check_checksum() {
  local file="$1" sidecar="$2" name expected actual
  name="$(basename "$file")"
  read -r expected sidecar_name < "$sidecar"
  [[ "$sidecar_name" == "$name" && "$expected" =~ ^[[:xdigit:]]{64}$ ]]
  actual="$(sha256sum "$file" | awk '{print $1}')"
  actual="$(printf '%s' "$actual" | tr '[:upper:]' '[:lower:]')"
  expected="$(printf '%s' "$expected" | tr '[:upper:]' '[:lower:]')"
  [[ "$actual" == "$expected" ]]
}

gh release download v0.1.2 --repo "$GITHUB_REPOSITORY" \
  --pattern "$old_name" --pattern "$old_name.sha256" --dir "$old_dir"
check_checksum "$old_dir/$old_name" "$old_dir/$old_name.sha256"
check_checksum "$current_dir/$current_name" "$current_dir/$current_name.sha256"

docker run --rm \
  --volume "$old_dir:/old:ro" \
  --volume "$current_dir:/new:ro" \
  --volume "$GITHUB_WORKSPACE/packaging/validate-install-arch-container.sh:/validate-install.sh:ro" \
  --volume "$GITHUB_WORKSPACE/packaging/validate-install-database.py:/validate-install-database.py:ro" \
  --env CURRENT_VERSION="$version" \
  --env SOURCE_SHA="$GITHUB_SHA" \
  archlinux:base-devel bash /validate-install.sh
