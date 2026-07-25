# WASI-Only Extension Packages

## Goal

Threadlane extensions are sandboxed WASI modules only. Remove native full-trust
execution, executable-revision trust state, and approval controls.

## Package Contract

An extension package is a directory containing:

- `threadlane-package.json`
- one declared `.wasm` module

The manifest retains package identity and descriptive metadata and declares one
WASI module path. It no longer accepts a native executable. Skill and prompt
packages remain a separate discovery concern and are not managed by the
extension screen.

## Installation and Discovery

Project-scoped installation validates the manifest, resolves the declared module
inside the selected source directory, verifies that it is a regular `.wasm`
file, and copies it to:

```text
<project>/.threadlane/extensions/<package-id>/extension.wasm
```

The existing `WasiExtensionManager` already discovers this directory layout.
Removing a package deletes only its resolved package directory. Refresh
rediscovers installed WASI modules through the existing capability catalog.

Package IDs and declared module paths must not escape their allowed roots.
Installation failures leave any existing installed package intact.

## Removed Surface

- `FullTrustRunner`
- `TrustStore` and persisted `state/trust.json` handling
- executable revision hashing
- `full_trust_executable` manifest support
- full-trust catalog metadata
- Approve/Revoke UI and confirmation state
- full-trust tests and repository guidance

Existing local trust files are ignored after removal; Threadlane does not need
to delete user data.

## UI

Rename the page to **WASI Extensions**. Show installed package name, scope, and
module status. Keep:

- Install folder
- Remove
- Refresh

Remove Package ID trust instructions and all Approve/Revoke language.

## Verification

- Installation rejects missing, non-WASM, absolute, and escaping module paths.
- Installation and removal operate only under the selected project extension
  root.
- Installed modules are discovered by `WasiExtensionManager`.
- Existing LSP and broker-smoke WASI tests continue to pass.
- `cargo check -p threadlane`, focused package/WASI tests, workspace Clippy,
  Hawk, and `git diff --check` pass.
