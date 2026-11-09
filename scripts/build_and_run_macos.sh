#!/usr/bin/env bash
# Build locally, keep only target/space, and launch it.
set -euo pipefail

if [[ "$(uname -s)" != Darwin ]]; then
  echo "This script requires macOS." >&2
  exit 1
fi

cd "$(dirname "$0")/.."
target_dir="$PWD/target"
if [[ -L "$target_dir" || -d "$target_dir/space" ]]; then
  echo "Refusing cleanup: target is a symlink or target/space is a directory." >&2
  exit 1
fi

host=$(rustc -vV | sed -n 's/^host: //p')
case "$host" in
  aarch64-apple-darwin|x86_64-apple-darwin) ;;
  *) echo "Unsupported macOS Rust host: $host" >&2; exit 1 ;;
esac

cargo build --locked --release --package space-app --bin space \
  --target "$host" --target-dir "$target_dir"

# Only discard build artifacts once the new executable is ready.
mv -f "$target_dir/$host/release/space" "$target_dir/space"
find "$target_dir" -mindepth 1 -maxdepth 1 ! -name space -exec rm -rf -- {} +

printf 'Build complete. Kept only %s; launching Space.\n' "$target_dir/space"
exec "$target_dir/space" "$@"
