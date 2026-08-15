# Threadlane GPUI

The GPUI frontend is organized as a layered application:

- `app/` — user actions and application coordination
- `state/` — in-memory UI/application state
- `screens/` — workspace, sidebar, chat, and settings views
- `components/` — reusable presentation primitives
- `services/` — boundaries around sessions, projects, and agent execution
- `adapters/` — backend event and model translation
- `persistence/` — project registry and local preferences

Views should dispatch `app::actions::AppAction` values rather than embedding
backend orchestration. Backend crates remain independent of GPUI. Prefer
Prefer controls from `gpui-component` over hand-built interactive `div` elements. Use `ActiveTheme` and `cx.theme().colors` for surfaces, borders, text, and interaction states; do not introduce one-off UI color literals.

## Themes

Threadlane uses the standard `gpui-component` `ThemeRegistry` and `ThemeConfig` flow.
The bundled theme lives in `themes/threadlane.json`; additional theme-set JSON files
can be placed in `~/.threadlane/themes/` and are watched at runtime. The selected
theme is persisted in `~/.threadlane/gui/preferences.json` and can be changed from
Settings. UI code must use semantic values from `cx.theme().colors` so switching a
theme repaints the entire application.

## Validation

```bash
cargo check -p threadlane-gpui
cargo test -p threadlane-gpui
```

## Releases

`.github/workflows/release.yml` is the canonical Threadlane release pipeline.
It packages the GPUI app from tags named `vX.Y.Z`, including tags created by
Release Please. The tag version must match the workspace version in the root
`Cargo.toml`. The workflow builds Linux x86_64 and arm64 archives, plus an
ad-hoc-signed Apple Silicon macOS DMG and ZIP, then uploads them to the GitHub
release. No Apple Developer ID or notarization secrets are currently required.
Automatic updates still require the repository variable
`THREADLANE_UPDATER_PUBLIC_KEY` and the updater-only secrets
`CARGO_PACKAGER_SIGN_PRIVATE_KEY` and
`CARGO_PACKAGER_SIGN_PRIVATE_KEY_PASSWORD`; these authenticate update archives
and are separate from Apple code signing.

Local bundle commands (run from the repository root):

```bash
./scripts/bundle-gpui-linux.sh
brew install create-dmg
./scripts/bundle-gpui-macos.sh
```

The local macOS command and CI both use ad-hoc signing. Because the artifacts
are not Developer ID signed or notarized, macOS Gatekeeper may require users to
explicitly approve the application when opening it for the first time.
