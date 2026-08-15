#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

target_dir="${CARGO_TARGET_DIR:-target}"
version="$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name == "threadlane-gpui") | .version')"
target_triple="$(rustc -vV | sed -n 's/^host: //p')"
package="threadlane-${version}-${target_triple}"
archive="$target_dir/release/$package.tar.gz"
staging="$(mktemp -d)"
trap 'rm -rf -- "$staging"' EXIT

cargo build --locked --release --bin threadlane-gpui

package_dir="$staging/$package"
install -Dm755 "$target_dir/release/threadlane-gpui" "$package_dir/bin/threadlane"
install -Dm644 packaging/threadlane.desktop \
  "$package_dir/share/applications/dev.threadlane.app.desktop"
install -Dm644 resources/icon_256.png \
  "$package_dir/share/icons/hicolor/256x256/apps/threadlane.png"

mkdir -p "$(dirname "$archive")"
tar -C "$staging" -czf "$archive" "$package"
printf 'Created %s\n' "$archive"
