#!/usr/bin/env bash
set -euo pipefail

# Keep the roadmap honest: the documented CLI/TUI surface is not present in
# this checkout, so fail loudly until the binary is added rather than claiming
# parity by inference from the desktop app.
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ ! -f "$root/crates/threadlane-cli/Cargo.toml" ]]; then
  echo "surface parity blocked: crates/threadlane-cli is not present" >&2
  exit 1
fi

echo "surface parity: CLI crate present"
