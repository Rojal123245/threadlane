# Task 1 Report: Secure OpenAI key storage

Implemented secure standalone OpenAI API key storage in `crates/threadlane-auth/src/openai_auth.rs`.

What changed:

- Added `load_openai_api_key() -> Option<String>`.
- Added `save_openai_api_key(key: &str) -> Result<(), String>`.
- Stored the key inside the existing `~/.threadlane` boundary without changing Codex credential loading behavior.
- Rejected empty or whitespace-only keys.
- Wrote the key with restrictive permissions on Unix (`0600`) and kept errors generic so the key is never echoed back.
- Left `load_credentials()` and the existing Codex fallback logic intact.

Tests added:

- OpenAI key save/load round trip.
- Empty-key rejection.
- Unix file-permission check for stored keys.
- Generic write-error handling that does not include the secret value.
- Regression coverage that Codex `~/.codex/auth.json` fallback still works.

Verification:

- `cargo test -p threadlane-auth` ✅
- `git -c core.fsmonitor=false diff --check` ✅

Notes:

- The repo already had an unrelated untracked plan file under `docs/superpowers/plans/2026-08-02-threadlane-cli-login.md`; I left it untouched.

## Round 1 Fix Notes

Reviewer feedback addressed:

- Closed the OpenAI key file permission window by writing to a temp file created with restrictive permissions and then renaming it into place.
- Restored `HOME` after each test via an RAII guard, and kept test execution serialized with a crate-local mutex.
- Kept Codex fallback behavior unchanged.

Verification after the fix:

- `cargo test -p threadlane-auth` ✅
- `git -c core.fsmonitor=false diff --check` ✅

## Round 3 Fix Notes

Reviewer feedback addressed:

- Removed the non-Unix `.bak` fallback entirely.
- Preserved the canonical key path without removing the live file first.
- Kept the secure temp-file + `0600` Unix path and used a direct truncate/write portable fallback without backup files.
- Added a regression test that confirms repeated saves do not leave an `.bak` path behind.

Verification after the fix:

- `cargo test -p threadlane-auth` ✅
- `git -c core.fsmonitor=false diff --check` ✅

## Round 2 Fix Notes

Reviewer feedback addressed:

- Removed the destination-delete step before replacement.
- Kept the secure temp-file write with `0600` before write.
- Preserved the prior key until replacement succeeds by using atomic rename on Unix and a backup-based fallback on non-Unix targets.

Verification after the fix:

- `cargo test -p threadlane-auth` ✅
- `git -c core.fsmonitor=false diff --check` ✅

## Round 4 Fix Notes

Reviewer feedback addressed:

- Replaced the non-Unix in-place truncate/write fallback with the same staged write path used on Unix.
- Reused the repository's native Windows `MoveFileExW` replacement pattern with replace-existing and write-through flags; no dependency was added.
- Kept the previous canonical key untouched until the staged file is fully written, synced, and successfully replaced, and removed the staged file on failure without creating a cleartext `.bak`.
- Preserved Unix `0600` creation before writing, `TestHomeGuard`, and existing Codex credential loading behavior.
- Added failure-injection coverage proving a replacement error preserves the prior key, and made overwrite/no-backup coverage platform-independent.

Verification after the fix:

- `cargo test -p threadlane-auth` ✅ (13 passed)
- `git diff --check` ✅
