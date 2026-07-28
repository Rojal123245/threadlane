# WASI-Only Extension Packages Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove native full-trust extensions and make the package manager install one sandboxed WASI module per project-scoped extension package.

**Architecture:** `PackageManager` validates a strict extension-package manifest and installs its declared module into the directory layout already consumed by `WasiExtensionManager`. The capability catalog and desktop UI report only installed WASI packages; native executable runners, trust persistence, and approval controls are deleted.

**Tech Stack:** Rust, Serde, Makepad, existing `WasiExtensionManager`, Cargo tests, Hawk.

## Global Constraints

- An extension package contains `threadlane-package.json` and exactly one declared `.wasm` module.
- Install packages only under `<project>/.threadlane/extensions/<package-id>/`.
- Reject absolute paths, path traversal, missing modules, non-files, and non-`.wasm` modules.
- A failed replacement must preserve the previously installed package.
- Do not delete legacy local `state/trust.json` files; stop reading them.
- Skill and prompt package discovery remains unchanged.
- Add no dependencies or lint allowances.

---

## File Map

- `crates/threadlane-coding-agent/src/packages.rs` — strict WASI package manifest, validation, project installation/removal, and installed-package discovery.
- `crates/threadlane-coding-agent/src/capabilities.rs` — WASI-only capability catalog.
- `crates/threadlane-coding-agent/src/full_trust_extension.rs` — delete.
- `crates/threadlane-coding-agent/src/lib.rs` — remove full-trust and obsolete package exports.
- `crates/threadlane-coding-agent/tests/supervisor_tests.rs` — package lifecycle and rejection coverage.
- `crates/threadlane-coding-agent/tests/wasi_tests.rs` — installed-package discovery coverage.
- `crates/threadlane/src/state.rs` — WASI package rows only.
- `crates/threadlane/src/app/mod.rs` — WASI-only package screen and actions.
- `AGENTS.md` — replace full-trust guidance with the WASI-only package invariant.

---

### Task 1: Define and validate the WASI package contract

**Files:**
- Modify: `crates/threadlane-coding-agent/src/packages.rs`
- Modify: `crates/threadlane-coding-agent/tests/supervisor_tests.rs`
- Modify: `crates/threadlane-coding-agent/tests/wasi_tests.rs`

**Interfaces:**
- Produces stateless `PackageManager::new() -> Self`.
- Produces `PackageManager::install_from_local(source: &Path, project_root: &Path) -> Result<PackageRecord, String>`.
- Produces `PackageManager::remove_package(package_id: &str, project_root: &Path) -> Result<(), String>`.
- Produces `PackageManager::list_packages(project_root: &Path) -> Vec<PackageRecord>` for the catalog.
- `PackageRecord` exposes `id()`, `name()`, `module_path()`, and `is_enabled()`.
- Installed output is consumed unchanged by `WasiExtensionManager::discover_and_load(project_root)`.

- [ ] **Step 1: Replace the package lifecycle fixture with a WASI package fixture**

Create a source directory containing `extension.wasm` and:

```json
{
  "id": "test-extension",
  "name": "Test Extension",
  "version": "1.0.0",
  "description": "test fixture",
  "extension": "extension.wasm"
}
```

Assert that installation creates:

```text
<project>/.threadlane/extensions/test-extension/threadlane-package.json
<project>/.threadlane/extensions/test-extension/extension.wasm
```

Assert the installed record has ID `test-extension`, module path
`<project>/.threadlane/extensions/test-extension/extension.wasm`, and is
removed by `remove_package`.

- [ ] **Step 2: Add rejection and replacement-preservation tests**

Use table cases for these manifest values:

```text
"../outside.wasm"
"/tmp/outside.wasm"
"extension.bin"
"missing.wasm"
```

Each call must return `Err` and create nothing below
`<project>/.threadlane/extensions/`. Add a separate case that first installs
valid bytes, attempts an invalid replacement of the same ID, and asserts the
original installed `extension.wasm` bytes remain unchanged.

- [ ] **Step 3: Add the package-to-WASI-loader integration test**

Use the existing minimal WASM fixture bytes and a valid package manifest.
Install the package, then:

```rust
let mut extensions = WasiExtensionManager::for_project(project.path());
assert_eq!(extensions.discover_and_load(project.path()), 1);
assert!(extensions.get_extensions().contains_key("test_extension"));
```

Use the manifest name encoded by the WASM fixture for the final assertion.

- [ ] **Step 4: Run the tests and verify RED**

Run:

```bash
cargo test -p threadlane-coding-agent --test supervisor_tests package
cargo test -p threadlane-coding-agent --test wasi_tests installed_package
```

Expected: compilation fails because the current API takes `PackageScope` and
the current manifest has no required `extension` field.

- [ ] **Step 5: Implement the strict manifest and project-only paths**

Use a strict manifest:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageManifest {
    id: String,
    name: String,
    version: String,
    description: Option<String>,
    extension: PathBuf,
}
```

Make `PackageManager` stateless; all installation and removal roots come from
the explicit `project_root` argument.

Validate package IDs as non-empty ASCII alphanumeric strings allowing only
`-`, `_`, and `.` after the first character. Canonicalize the source directory
and declared module, require the module to remain below the source directory,
require `is_file()`, and require extension `wasm`.

Stage the normalized manifest and module in a sibling temporary directory. For
replacement, rename the current package directory to a backup, rename the
staged directory into place, restore the backup if that rename fails, and
remove the backup only after success. Normalize the installed module name to
`extension.wasm`.

- [ ] **Step 6: Run the package and loader tests and verify GREEN**

Run:

```bash
cargo test -p threadlane-coding-agent --test supervisor_tests package
cargo test -p threadlane-coding-agent --test wasi_tests installed_package
```

Expected: all package lifecycle, validation, and preservation cases pass.

---

### Task 2: Delete native execution and trust

**Files:**
- Delete: `crates/threadlane-coding-agent/src/full_trust_extension.rs`
- Modify: `crates/threadlane-coding-agent/src/lib.rs`
- Modify: `crates/threadlane-coding-agent/src/capabilities.rs`
- Modify: `crates/threadlane-coding-agent/tests/supervisor_tests.rs`

**Interfaces:**
- Removes `FullTrustRunner`, `TrustStore`, executable revision hashing, and `full_trust_executable`.
- Changes discovery to `CapabilityCatalog::discover(project_root: Option<&Path>) -> Self`.
- Retains `CapabilityCatalog::{packages, extensions}` with WASI-only extension metadata.

- [ ] **Step 1: Remove the full-trust test**

Delete `test_full_trust_revision_approval`. Keep package and supervisor tests
unrelated to native execution.

- [ ] **Step 2: Delete the implementation and exports**

Delete `full_trust_extension.rs`, remove `pub mod full_trust_extension`, and
remove the `FullTrustRunner`/`TrustStore` re-export. Remove `PackageScope` if
Task 1 leaves no callers.

- [ ] **Step 3: Simplify the capability catalog**

Remove trust-file loading and all native-executable synthesis. Reduce extension
metadata to fields actually sourced from loaded WASI manifests:

```rust
pub struct ExtensionMetadata {
    id: String,
    name: String,
    enabled: bool,
}
```

Discover package records from the active project extension root and WASI
extensions through `WasiExtensionManager`. Do not scan or mutate
`state/trust.json`.

- [ ] **Step 4: Run coding-agent tests**

Run:

```bash
cargo test -p threadlane-coding-agent
```

Expected: PASS with no references to full trust or native executables.

---

### Task 3: Simplify the desktop screen to WASI packages

**Files:**
- Modify: `crates/threadlane/src/state.rs`
- Modify: `crates/threadlane/src/app/mod.rs`
- Test: `crates/threadlane/src/state.rs`

**Interfaces:**
- Consumes project-only `PackageManager` methods and WASI-only `CapabilityCatalog`.
- Produces Install folder, Remove, and Refresh actions only.

- [ ] **Step 1: Replace the reducer test**

Delete trust-revocation assertions. Add a package refresh assertion that checks
the reducer copies package ID, name, module path, and enabled state from a
catalog discovered from a temporary project extension package.

- [ ] **Step 2: Run the focused test and verify RED**

Run:

```bash
cargo test -p threadlane capability
```

Expected: FAIL while `CapabilityState` still expects trust/full-trust fields.

- [ ] **Step 3: Remove trust state and handlers**

Delete:

```text
pending_trust_action
confirm_trust_change
CapabilityState::mark_revoked
CapabilityExtensionRow.full_trust
CapabilityExtensionRow.revision
CapabilityExtensionRow.trusted
```

Change install/remove calls to the project-only `PackageManager` signatures.
Keep folder selection and refresh event routing unchanged.

- [ ] **Step 4: Simplify the Makepad page**

Rename navigation and page labels from `Capabilities` to `WASI Extensions`.
Use description:

```text
Install and manage sandboxed WASI extension packages for this project.
```

Remove Approve and Revoke buttons and their action handling. Keep the package
ID input because removal uses it. Render each package as:

```text
<name> · project · WASI
```

- [ ] **Step 5: Run the focused test and compile**

Run:

```bash
cargo test -p threadlane capability
cargo check -p threadlane
```

Expected: PASS with no workspace-owned warnings.

---

### Task 4: Remove stale guidance and validate

**Files:**
- Modify: `AGENTS.md`
- Modify: `docs/superpowers/plans/2026-07-25-wire-dormant-capabilities.md`

**Interfaces:**
- Documents that extensions are WASI-only and LSP uses brokered process capability.

- [ ] **Step 1: Update repository guidance**

Replace full-trust approval guidance with:

```text
Threadlane extensions are WASI modules. Extension packages install one declared
module under `.threadlane/extensions/<package-id>/extension.wasm`; native
extension executables and trust approvals are unsupported. LSP remains a WASI
extension and launches language servers through brokered process capability.
```

Mark the old plan’s full-trust UI language as superseded by the WASI-only
design rather than leaving contradictory instructions.

- [ ] **Step 2: Confirm no native-extension surface remains**

Run:

```bash
rg -n "FullTrust|full_trust|TrustStore|native executable|Approve|Revoke" \
  crates/threadlane-coding-agent crates/threadlane AGENTS.md
```

Expected: no matches related to extension execution or UI.

- [ ] **Step 3: Run the full validation set**

Run:

```bash
cargo test -p threadlane-coding-agent
cargo test -p threadlane capability
cargo check -p threadlane
cargo clippy --workspace --all-targets --all-features -- -D warnings
just hawkcheck
git diff --check
```

Expected: every command exits 0 and Hawk reports 0 findings.
