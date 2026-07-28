# Global and Project WASI Extension Manager

## Goal

Manage only sandboxed WASI extensions, show both global and project extensions
in Provider Settings, and make existing loose modules such as
`.threadlane/extensions/lsp_ext.wasm` visible.

## Installation model

Threadlane installs an already compiled `.wasm` file. It does not run Cargo or
execute build scripts. The module's exported `extension_info` manifest supplies
its name, version, and capabilities.

The add control chooses a destination scope:

- Global: `~/.threadlane/extensions/<extension-name>.wasm`
- Project: `<project>/.threadlane/extensions/<extension-name>.wasm`

Rust extension authors build first with:

```bash
cargo build --target wasm32-wasip1 --release
```

They then select the resulting file under
`target/wasm32-wasip1/release/`.

## Discovery and precedence

One inventory discovers loose `.wasm` files and the existing managed
`<id>/extension.wasm` layout in both roots. Each row records the manifest name
and version, filesystem path, scope, enabled state, and whether it is the
effective runtime module.

Global modules load first. An enabled project module with the same manifest
name overrides the global module. Both remain visible in settings; the global
row is labelled as overridden.

## Enable, disable, and removal

Enabled state is scope-specific and persisted with an adjacent
`<module>.disabled` marker. Disabling a project override allows an enabled
global module with the same name to become effective. The module itself is
never renamed.

Removal targets the exact inventoried module after validating that it remains
directly within its declared extension root. Managed legacy directories are
removed as a unit; loose modules remove only the module and its marker.

## Settings UI

The WASI Extensions page contains:

- a compact `Global` / `Project` install-scope selector;
- top-right add and refresh icon buttons;
- installation guidance explaining that the picker expects compiled `.wasm`;
- one row per discovered extension with name, version, scope, source path,
  active/overridden state, enable toggle, and remove icon;
- an empty state only when neither scope contains an extension.

The Package ID input and package-folder controls are removed.

## Validation

Backend tests cover both roots, loose and legacy managed layouts, project
precedence, independent disable markers, direct-WASM installation, replacement,
removal containment, and invalid WASM rejection. Desktop tests cover projection
of both scopes and row state. Compilation, strict Clippy, Hawk, and diff checks
remain required.
