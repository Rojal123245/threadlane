# Provider Settings Navigation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the compact provider card modal with a scalable settings workspace containing a left navigation and provider/About detail pages, with Google Antigravity selected by default.

**Architecture:** Keep the existing `ProviderSettingsModal` overlay and `App` provider action/event flow. Add local page-selection state to the modal, build the navigation and detail surfaces in the existing Makepad script component, and conditionally draw the selected page using visibility toggles synchronized from Rust. Preserve existing provider widget IDs so current action handling continues to work.

**Tech Stack:** Rust 2021, Makepad widgets and `script_mod!`, existing `ProviderSettingsModal`, existing provider auth APIs and `App` action handling.

## Global Constraints

- Preserve existing provider authentication, diagnostics, model routing, persistence, and chat behavior.
- Google Antigravity is the default page every time the modal opens.
- Navigation selection changes presentation only and never changes the active chat model.
- Keep the full-window blurred backdrop and existing outside-click, Escape, and BackPressed dismissal behavior.
- Use real navigation buttons with selected, hover, focus, and pressed states; category labels are non-interactive.
- Display the About version using `env!("CARGO_PKG_VERSION")`, not a duplicated literal.
- Do not edit generated content under `target/`, `crates/threadlane/dist/`, or `.threadlane/`.
- Validate Rust/Makepad changes with `cargo check -p threadlane` and `git diff --check`.

---

### Task 1: Add explicit settings page state

**Files:**
- Modify: `crates/threadlane/src/app/mod.rs:2089-2099` (`ProviderSettingsModal`)
- Modify: `crates/threadlane/src/app/mod.rs:2163-2182` (`open`/`close`)

**Interfaces:**
- Produces `SettingsPage::{GoogleAntigravity, OpenAi, About}` and a `page` field on `ProviderSettingsModal`.
- Produces a modal method that synchronizes page visibility after selection or opening.

- [ ] **Step 1: Define the page enum immediately before `ProviderSettingsModal`.**

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsPage {
    GoogleAntigravity,
    OpenAi,
    About,
}
```

- [ ] **Step 2: Add the default page field to `ProviderSettingsModal`.**

```rust
#[rust]
page: SettingsPage,
```

Use the existing Makepad widget initialization behavior. Add `impl Default for SettingsPage` returning `SettingsPage::GoogleAntigravity`; the derived modal field then starts on the first provider without requiring a script property.

- [ ] **Step 3: Add page-selection and synchronization methods.**

The methods must:

```rust
fn set_page(&mut self, cx: &mut Cx, page: SettingsPage) {
    self.page = page;
    self.sync_page_visibility(cx);
    self.view.redraw(cx);
}
```

`sync_page_visibility` must set the selected navigation button and detail-page visibility using typed widget references. Use `button(...).set_visible(...)` for buttons and `widget(...).set_visible(...)` only for non-button containers. The selected page must be Google Antigravity after `open` resets `self.page`.

- [ ] **Step 4: Reset the page in `open`.**

Before setting `opened = true`, assign:

```rust
self.page = SettingsPage::GoogleAntigravity;
```

Then sync visibility after the view is available and redraw the overlay/view as today.

- [ ] **Step 5: Run the focused compiler check.**

Run: `cargo check -p threadlane`
Expected: It may fail until the new IDs are added in Task 2; record only actual errors and continue with the script changes.

---

### Task 2: Replace the compact modal script with a settings workspace

**Files:**
- Modify: `crates/threadlane/src/app/mod.rs:1235-1478` (`modal_card` script)

**Interfaces:**
- Produces these stable IDs for Rust synchronization and existing App action handling:
  - `settings_nav_google_btn`
  - `settings_nav_openai_btn`
  - `settings_nav_about_btn`
  - `google_antigravity_page`
  - `openai_page`
  - `about_page`
  - Existing provider action IDs: `antigravity_login_btn`, `antigravity_doctor_btn`, `openai_login_btn`.
- Keeps `modal_card` and `close_modal_btn` unchanged in purpose so outside-click and close handling remain valid.

- [ ] **Step 1: Expand the modal card and convert it to a horizontal shell.**

Use this exact starting structure, adjusting only indentation to match the surrounding DSL:

```text
modal_card := RoundedView {
    width: 780
    height: 520
    flow: Right
    padding: 0
    spacing: 0
    draw_bg +: {
        color: #x1a1d24
        border_radius: 12.0
        border_size: 1.0
        border_color: #x323a48
    }
}
```

Keep the close control inside the right content header so the modal remains dismissible and the left navigation stays stable.

- [ ] **Step 2: Add the left settings navigation column.**

Create a fixed-width column around 180 px with padding and a subtle right divider. Use category labels for `PROVIDERS` and `ADVANCED`, then real buttons for the three entries. Each button must define `color`, `color_hover`, `color_focus`, and `color_down`, plus matching border-state colors if a border is present. Navigation buttons are icon-free text buttons and should have explicit padding, spacing, and alignment.

Use these labels exactly:

```text
PROVIDERS
Google Antigravity
OpenAI / ChatGPT

ADVANCED
About
```

- [ ] **Step 3: Add the right content column and shared header.**

Create a flexible right column with padding. Add a header containing a title label and the existing `close_modal_btn`. Keep the subtitle/description inside each selected page rather than showing both providers at once.

- [ ] **Step 4: Move Antigravity controls into `google_antigravity_page`.**

Preserve the existing IDs and text/action behavior:

- `antigravity_status_lbl`
- `antigravity_login_btn`
- `antigravity_doctor_btn`

Retain the provider description and card styling, but size it for the wider detail column.

- [ ] **Step 5: Move OpenAI controls into `openai_page`.**

Preserve:

- `openai_status_lbl`
- `openai_login_btn`

Keep the existing managed-external and disconnect text behavior controlled by `refresh_provider_connection_ui`.

- [ ] **Step 6: Add `about_page`.**

Add an About page containing the application description and a version label whose script text is supplied from Rust using `env!("CARGO_PKG_VERSION")` or a compile-time `concat!` expression. Do not hardcode the package version number.

- [ ] **Step 7: Keep inactive pages non-interactive.**

The page containers must be hidden, not merely transparent, when inactive. This prevents hidden full-size content from intercepting pointer events. The active page must occupy the right content area without changing the modal card bounds.

---

### Task 3: Wire navigation events locally in the modal

**Files:**
- Modify: `crates/threadlane/src/app/mod.rs:2121-2138` (`ProviderSettingsModal::handle_event`)

**Interfaces:**
- Consumes the navigation button IDs from Task 2.
- Calls `set_page(cx, SettingsPage::...)` after nested UI event dispatch.
- Does not change `App::handle_actions` provider action routing.

- [ ] **Step 1: Dispatch existing nested events first.**

Keep:

```rust
self.view.handle_event(cx, event, scope);
```

before checking navigation actions so the buttons receive standard Makepad button actions.

- [ ] **Step 2: Handle typed navigation button actions.**

Inside `if let Event::Actions(actions) = event`, inspect the typed button refs:

```rust
if self.view.button(cx, ids!(settings_nav_google_btn)).clicked(actions) {
    self.set_page(cx, SettingsPage::GoogleAntigravity);
}
if self.view.button(cx, ids!(settings_nav_openai_btn)).clicked(actions) {
    self.set_page(cx, SettingsPage::OpenAi);
}
if self.view.button(cx, ids!(settings_nav_about_btn)).clicked(actions) {
    self.set_page(cx, SettingsPage::About);
}
```

Use the typed button references shown above; do not replace them with raw pointer coordinates or root-level lookup of nested controls.

- [ ] **Step 3: Preserve dismissal after navigation handling.**

Keep the existing outside-click, Escape, and BackPressed logic. Ensure navigation clicks inside `modal_card` do not trigger dismissal.

- [ ] **Step 4: Synchronize initial visibility.**

Call `sync_page_visibility` from `open` and from `on_after_apply` after the script has been applied. The first visible page must be Google Antigravity, with its navigation button selected.

---

### Task 4: Synchronize About content and provider refresh behavior

**Files:**
- Modify: `crates/threadlane/src/app/mod.rs` in the modal script and the existing `ProviderSettingsModal` initialization methods

**Interfaces:**
- About version is compile-time sourced.
- Existing `refresh_provider_connection_ui` continues to find and update provider labels/buttons by their preserved IDs.

- [ ] **Step 1: Verify every existing provider ID remains unique.**

Search the file for each of:

```text
antigravity_status_lbl
antigravity_login_btn
antigravity_doctor_btn
openai_status_lbl
openai_login_btn
```

Each must occur exactly once in the active modal script, unless the repository’s widget lookup explicitly supports duplicated template IDs.

- [ ] **Step 2: Set About version from package metadata.**

Add an `about_version_lbl` label with initial text `Version`. In `ProviderSettingsModal::on_after_apply`, after the existing draw-list redraw, set it with the existing Makepad label API:

```rust
let version = format!("Version {}", env!("CARGO_PKG_VERSION"));
self.view.label(cx, ids!(about_version_lbl)).set_text(cx, &version);
```

Use `vm.with_cx_mut` for this update, matching the existing `on_after_apply` implementation. Do not introduce runtime filesystem or network access for About.

- [ ] **Step 3: Re-check provider event paths.**

Confirm that login success/error, disconnect, and health-check action branches still target the preserved IDs and that no page selection action is forwarded to `App` as a provider action.

- [ ] **Step 4: Run focused validation.**

Run:

```bash
cargo check -p threadlane
git diff --check
```

Expected: both commands succeed. Cargo’s existing duplicate-package warnings are acceptable if there are no errors.

---

### Task 5: Review and manually verify the settings workspace

**Files:**
- Review: `crates/threadlane/src/app/mod.rs`
- Review: `docs/superpowers/specs/2026-07-25-provider-settings-navigation-design.md`

- [ ] **Step 1: Inspect the complete diff.**

Run:

```bash
git diff -- crates/threadlane/src/app/mod.rs
git diff --check
git status --short
```

Confirm the pre-existing Gauss blur change is preserved and no unrelated files changed.

- [ ] **Step 2: Run the application.**

Run from the repository root:

```bash
cargo run -p threadlane
```

Manually verify:

1. Settings opens as a wide workspace.
2. Google Antigravity is selected by default.
3. OpenAI / ChatGPT switches the right panel without changing the chat model.
4. About displays the Threadlane description and current version.
5. Existing provider status text and buttons update correctly.
6. Login, disconnect, and health check controls retain their current behavior.
7. Close button, outside click, Escape, and BackPressed still dismiss the modal.
8. Hidden pages do not intercept pointer input.
9. Navigation hover/focus/pressed states are visible and aligned.

- [ ] **Step 3: Record visual limitations.**

If the application cannot be launched in the environment, report that compilation and diff checks passed but visual runtime verification remains outstanding. Do not claim visual verification without observing the running application.

- [ ] **Step 4: Commit the implementation.**

After validation succeeds:

```bash
git add crates/threadlane/src/app/mod.rs
git commit -m "feat: add settings navigation workspace"
```

Do not amend the previously committed design document commit.
