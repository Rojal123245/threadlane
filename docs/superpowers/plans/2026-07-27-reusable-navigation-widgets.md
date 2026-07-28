# Reusable Navigation Widgets Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract shared navigation-button behavior so settings and future app navigation entries use identical selected, hover, focus, pressed, border, and text states.

**Architecture:** Add a reusable `NavButton` script prototype under `crates/threadlane/src/components/` with one visual contract and a small Rust helper for selected state. Keep each control as a native Makepad `Button` so existing IDs and `ButtonAction` routing remain unchanged. Migrate the provider-settings sidebar first, then audit existing navigation-like buttons before expanding the component’s scope.

**Tech Stack:** Rust, Makepad `script_mod!`, Makepad `Button`, existing `theme.*` semantic color tokens, `cargo check`, focused Rust tests.

## Global Constraints

- Keep all colors in `theme.*`; do not add component-specific color literals.
- Define `color`, `color_hover`, `color_focus`, and `color_down` together, including matching border and text states.
- Preserve existing `ProviderSettingsModalAction` routing and page visibility behavior.
- Register new component modules in both the Rust module list and `components::script_mod`.
- Do not introduce a generic mega-button; only unify controls with the same navigation semantics.
- Do not edit generated files under `target/`, `crates/threadlane/dist/`, or `.threadlane/`.

---

### Task 1: Define the reusable navigation-button contract

**Files:**
- Create: `crates/threadlane/src/components/nav_button.rs`
- Modify: `crates/threadlane/src/components/mod.rs`
- Modify: `AGENTS.md`

**Interfaces:**
- Produces script prototype `mod.components.NavButton` for text navigation entries.
- Produces Rust helper `nav_button::set_selected(cx: &mut Cx, button: &ButtonRef, selected: bool)`.
- `NavButton` remains a native `Button` prototype so existing `ButtonRef::clicked` action handling is preserved.

- [x] **Step 1: Add the component module and registration hooks**

Add `pub mod nav_button;` beside the other reusable primitives and call `nav_button::script_mod(vm);` from `components::script_mod`.

- [x] **Step 2: Define the minimal selected-state API**

Implement `set_selected` by playing the component’s `selected.on` or `selected.off` animator state. Each animator state must update all state layers when selection changes. The update must cover:

```text
draw_bg:     color, color_hover, color_focus, color_down,
             border_color, border_color_hover, border_color_focus, border_color_down
draw_text:   color, color_hover, color_focus, color_down
```

Use `theme.color_primary`, `theme.color_secondary`, `theme.color_card`, `theme.color_background`, `theme.color_foreground`, `theme.color_primary_foreground`, and `theme.color_border`; do not add new palette tokens.

- [x] **Step 3: Define the reusable script prototype**

Give `mod.components.NavButton` the shared geometry currently duplicated in the provider sidebar:

```text
width: Fill
height: 34
padding: Inset{left: 10 right: 8 top: 6 bottom: 6}
spacing: 0
align: Align{x: 0.0 y: 0.5}
border_radius: 6.0
```

Keep the label text supplied by each instance.

- [x] **Step 4: Document the boundary**

Add an AGENTS.md rule that navigation buttons must use `mod.components.NavButton` when they represent sibling navigation destinations; ordinary action buttons, icon buttons, and destructive controls remain separate variants.

- [x] **Step 5: Run registration and compile checks**

Run `rtk cargo check -p threadlane` and `rtk git diff --check`. Expected: no Makepad registration or script-property errors.

---

### Task 2: Migrate ProviderSettingsModal to NavButton

**Files:**
- Modify: `crates/threadlane/src/app/mod.rs:1350-1500`
- Modify: `crates/threadlane/src/app/mod.rs:3240-3300`

**Interfaces:**
- Consumes `mod.components.NavButton` and `nav_button::set_selected`.
- Preserves the existing five IDs: `settings_nav_google_btn`, `settings_nav_openai_btn`, `settings_nav_capabilities_btn`, `settings_nav_skills_btn`, and `settings_nav_about_btn`.

- [x] **Step 1: Replace duplicated Button definitions**

Change each provider-settings sidebar declaration to `mod.components.NavButton { text: "..." }`. Remove the per-instance `draw_bg` and `draw_text` blocks so no entry can drift from the shared state contract.

- [x] **Step 2: Replace partial runtime color mutation**

In `sync_page_visibility`, replace the `Vec4f` color construction and `script_apply_eval!` loop with `nav_button::set_selected(cx, &button, selected)` for the same five IDs. Keep visibility and page selection logic unchanged.

- [x] **Step 3: Preserve initial state synchronization**

Ensure `on_after_apply`, `open_page`, and `set_page` still invoke `sync_page_visibility`, so the first frame and every page switch use the same selected-state path.

- [x] **Step 4: Run focused verification**

Run:

```bash
rtk cargo check -p threadlane
rtk cargo test -p threadlane
rtk git diff --check
```

Expected: existing tests pass and no runtime theme/property errors appear during a local `rtk cargo run -p threadlane` parse phase.

---

### Task 3: Audit and migrate other navigation-like controls

**Files:**
- Inspect and modify only the confirmed navigation call sites under `crates/threadlane/src/app/mod.rs` and `crates/threadlane/src/components/`
- Modify: `AGENTS.md` only if a durable navigation convention is discovered

**Interfaces:**
- Consumes the completed `NavButton` component.
- Does not change action-button, icon-button, dropdown, context-menu, terminal-tab, or destructive-button behavior unless it is semantically a navigation destination.

- [x] **Step 1: Inventory candidates**

Review existing `Button` declarations for sibling destinations, especially settings, modal tabs, panel selectors, and sidebar section navigation. Exclude buttons whose primary behavior is submit, close, refresh, add, delete, login, or toggle.

- [x] **Step 2: Migrate only matching candidates**

Replace matching candidates with `mod.components.NavButton` and route their selected state through `nav_button::set_selected`. Keep each owner’s existing action handling and IDs.

- [x] **Step 3: Verify no partial state styling remains**

Search for navigation button definitions that set only `draw_bg.color` at runtime while leaving hover/focus/down or border states behind. Either migrate them or record why they intentionally use another component.

- [x] **Step 4: Run validation**

Run `rtk cargo check -p threadlane`, focused tests for touched logic, and `rtk git diff --check`.

---

### Task 4: Runtime visual verification and cleanup

**Files:**
- Modify only files needed to correct verified visual regressions.

- [ ] **Step 1: Run the app through the Makepad/Studio workflow**

Start a fresh run after the code changes and inspect the provider settings modal.

- [ ] **Step 2: Verify the state matrix**

For every sidebar entry, verify:

| State | Expected result |
|---|---|
| Normal | Same dark surface, border, and label treatment |
| Hover | Same hover surface and border treatment |
| Focus | Same focus treatment as hover/selected contract |
| Pressed | Same pressed treatment |
| Selected | Same persistent selected surface, border, and text treatment |
| Page switch | Previous entry returns to normal; new entry becomes selected |

- [x] **Step 3: Re-run final checks**

Run `rtk cargo check -p threadlane`, `rtk cargo test -p threadlane`, and `rtk git diff --check`. Report separately if local Metal initialization prevents full app observation after Makepad parsing.
