# Global and Project WASI Extension Manager Implementation Plan

> Execute with fresh implementation and review subagents, one task at a time.

**Goal:** Replace the manifest-folder package UI with one WASI extension
inventory that discovers, loads, installs, toggles, and removes compiled modules
from global and project scopes.

**Architecture:** Keep module parsing in `WasiExtension`; make `PackageManager`
the small filesystem lifecycle layer for compiled modules and make
`WasiExtensionManager` the runtime loader. Both consume the same scoped
inventory rules. Project modules override global modules by manifest name.

**Tech:** Rust, Makepad, Wasmi, serde, existing `rfd` picker and icon components.

---

## Task 1: Scoped extension lifecycle

**Files:**
- Modify: `crates/threadlane-coding-agent/src/packages.rs`
- Modify: `crates/threadlane-coding-agent/src/lib.rs`
- Test: `crates/threadlane-coding-agent/tests/supervisor_tests.rs`

1. Add failing tests for direct `.wasm` install into explicit global/project
   roots, replacement, enable markers, loose-module removal, managed-layout
   removal, invalid WASM, and destination containment.
2. Replace `threadlane-package.json` parsing with validation through
   `WasiExtension::load_from_file`.
3. Model scope, manifest metadata, module path, enabled state, layout, and
   effective status in one record.
4. Keep atomic staging/replacement and existing symlink containment checks.
5. Run the focused lifecycle tests.

## Task 2: Shared discovery and runtime precedence

**Files:**
- Modify: `crates/threadlane-coding-agent/src/wasi_extension.rs`
- Modify: `crates/threadlane-coding-agent/src/capabilities.rs`
- Modify: `crates/threadlane-coding-agent/src/coding_agent.rs`
- Test: `crates/threadlane-coding-agent/tests/wasi_tests.rs`

1. Add failing tests proving both roots and both layouts are inventoried, disabled
   modules are skipped, and enabled project modules override global modules.
2. Add explicit-root discovery APIs for deterministic tests and a production
   global-root helper.
3. Load global before project while retaining separate catalog rows and marking
   the effective module.
4. Route `CodingAgent` startup and `CapabilityCatalog` through the shared scoped
   discovery path.
5. Run focused WASI and catalog tests.

## Task 3: Desktop state and events

**Files:**
- Modify: `crates/threadlane/src/state.rs`
- Modify: `crates/threadlane/src/app/mod.rs`
- Test: `crates/threadlane/src/state.rs`

1. Add failing projection tests for global/project rows, enabled state,
   effective/overridden state, version, and path.
2. Replace package-folder events with `.wasm` file selection and explicit
   install scope.
3. Wire install, toggle, remove, and refresh to exact inventoried records.
4. Ensure mutations refresh both UI state and project capability cache.
5. Run focused desktop state tests.

## Task 4: Compact WASI Extensions UI

**Files:**
- Modify: `crates/threadlane/src/app/mod.rs`
- Reuse: `crates/threadlane/src/components/icon_button.rs`

1. Replace the Package ID/form controls with a header containing the scope
   selector plus compact add and refresh icon buttons.
2. Add build/install guidance for compiled `.wasm` selection.
3. Render bounded rows for both scopes with labels, enable controls, and
   per-row remove icons using existing Makepad patterns.
4. Keep all action hit-testing on the controls that own their visuals.
5. Run `cargo check -p threadlane`.

## Task 5: Documentation and full validation

**Files:**
- Modify: `README.md`
- Modify: `scripts/build_extensions.sh`
- Modify: `AGENTS.md` only if a durable repository rule was learned.

1. Stop the extension build script from clearing the shared project extension
   root so local installs and disabled markers survive bundled-extension builds.
2. Document global/project roots, direct-WASM installation, build command,
   precedence, and enable markers.
3. Run focused tests, `cargo test --workspace`, `cargo check -p threadlane`,
   strict Clippy, `cargo +1.97.1 hawk check`, and `git diff --check`.
4. Inspect the final UI at runtime when the Makepad Studio route is available;
   otherwise record the exact unverified visual surface.
5. Audit every requirement in the design before marking the goal complete.
