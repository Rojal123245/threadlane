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
