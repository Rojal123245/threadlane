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
