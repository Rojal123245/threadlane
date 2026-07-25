# Provider Settings Navigation Design

**Date:** 2026-07-25

## Goal

Turn the existing provider settings overlay into a scalable settings workspace with a fixed left navigation panel and a selected-page detail panel. Preserve all existing provider authentication, diagnostics, model routing, and modal dismissal behavior.

## Navigation

The settings workspace contains only these entries:

- **Providers** (non-interactive category)
  - Google Antigravity
  - OpenAI / ChatGPT
- **Advanced** (non-interactive category)
  - About

Google Antigravity is selected by default whenever the modal opens or reopens. Selecting a navigation item changes only the displayed settings page; it does not change the active chat model or provider.

## Layout

- Keep the existing full-window overlay and blurred backdrop.
- Expand the modal from its current compact card to a settings-sized card, approximately 760–820 px wide.
- Use a fixed left navigation column of approximately 180 px and a flexible right content column.
- Keep the close button and existing outside-click, Escape, and BackPressed dismissal behavior.
- Use real buttons for navigation entries with selected, hover, focus, and pressed states. Category labels remain non-interactive.

## Pages

### Google Antigravity

Move the existing Antigravity presentation into the detail panel:

- Provider title and description
- Existing connection status and account information
- Existing sign-in/disconnect action
- Existing Run Health Check action
- Existing model/provider information where already available

### OpenAI / ChatGPT

Move the existing OpenAI presentation into the detail panel:

- Provider title and description
- Existing connection status and credential source
- Existing sign-in/disconnect/managed externally behavior
- Existing model/provider information where already available

### About

Display:

- Threadlane application description
- Current version from `CARGO_PKG_VERSION`

About has no provider or network dependencies and is always available.

## State and architecture

Add a small local page-selection enum to `ProviderSettingsModal`:

```rust
enum SettingsPage {
    GoogleAntigravity,
    OpenAi,
    About,
}
```

The modal owns navigation selection and redraws its content. Existing `App` provider refresh and action handling remain the source of truth for credentials and asynchronous login/diagnostic flows. Existing provider action IDs should be retained where practical so no authentication behavior changes.

The first implementation keeps the page definitions in the existing script component. New reusable components are not required unless the resulting structure demands them.

## Interaction and error behavior

- Opening the modal resets selection to Google Antigravity.
- Provider actions retain current close, refresh, login, disconnect, and diagnostic behavior.
- Existing status and error labels remain unchanged.
- Navigation must not allow pointer events to leak into inactive content or underlying application widgets.
- No changes are made to provider credentials, model routing, persistence, or chat state.

## Validation

Run:

```bash
cargo check -p threadlane
git diff --check
```

Inspect the diff for preserved widget IDs, correct overlay instantiation, and unchanged provider action paths. Manually run the application to verify modal sizing, page switching, provider actions, About content, default selection, and visual alignment.
