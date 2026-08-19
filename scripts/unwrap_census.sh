#!/usr/bin/env bash
set -euo pipefail

# Report panic-prone constructs in the durable/runtime crates. This is a
# baseline metric: CI fails only when a touched-crate count increases.
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
crates=(
  "$root/crates/threadlane-agent"
  "$root/crates/threadlane-coding-agent"
  "$root/crates/threadlane-wasi"
)

for crate in "${crates[@]}"; do
  name="$(basename "$crate")"
  count="$(grep -RInE --include='*.rs' '\.(unwrap|expect)\s*\(|panic!\s*\(' "$crate/src" 2>/dev/null | wc -l | tr -d ' ')"
  printf '%s %s\n' "$name" "$count"
done
