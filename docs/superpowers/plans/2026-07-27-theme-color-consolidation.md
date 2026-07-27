# Theme Color Consolidation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Move every Threadlane UI color literal under `crates/threadlane/src` to the shared `theme.*` token table while preserving the current palette and documenting the rule.

**Architecture:** `crates/threadlane/src/theme/mod.rs` remains the only source of color literals. Script UI files consume semantic `theme.color_*` values; identical literals reuse one token, while genuinely distinct visual roles receive distinct semantic tokens. Rust-side UI colors are migrated only where the value is passed to a Makepad draw object and can consume a theme token without creating a second theme system.

**Tech Stack:** Rust, Makepad `script_mod!`, `cargo check`, ripgrep.

## Global Constraints

- Preserve existing color values and interaction-state differences.
- Keep new color literals confined to `crates/threadlane/src/theme/mod.rs`.
- Do not add dependencies or restructure the UI.
- Preserve explicit transparent border and hover/focus/down states.
- Run `cargo check -p threadlane` and `git diff --check` before completion.

---

### Task 1: Expand the semantic theme token table

**Files:**
- Modify: `crates/threadlane/src/theme/mod.rs`

**Interfaces:**
- Produces semantic `theme.color_*` names consumed by every UI script module.

- [x] Inventory each non-theme color literal by file and visual role; group exact duplicate values before adding tokens.
- [x] Add missing semantic and exact-palette tokens to `mod.theme`, keeping base palette literals at the top and grouped tokens below backgrounds, borders, text, accents, states, and status values.
- [x] Keep transparency and alpha-bearing colors as explicit tokens rather than dropping alpha.
- [x] Confirm every token name is valid in Makepad script scope and follows the existing `color_bg_*`, `color_border_*`, `color_text_*`, `color_accent_*`, `color_state_*` convention.

### Task 2: Migrate component and panel UI call sites

**Files:**
- Modify: `crates/threadlane/src/components/activity_header.rs`
- Modify: `crates/threadlane/src/components/activity_loader.rs`
- Modify: `crates/threadlane/src/components/auth_row.rs`
- Modify: `crates/threadlane/src/components/clipped_label.rs`
- Modify: `crates/threadlane/src/components/code_label.rs`
- Modify: `crates/threadlane/src/components/command_input.rs`
- Modify: `crates/threadlane/src/components/composer_action.rs`
- Modify: `crates/threadlane/src/components/composer_surface.rs`
- Modify: `crates/threadlane/src/components/context_menu.rs`
- Modify: `crates/threadlane/src/components/empty_row.rs`
- Modify: `crates/threadlane/src/components/icon_button.rs`
- Modify: `crates/threadlane/src/components/model_dropdown.rs`
- Modify: `crates/threadlane/src/components/notice_banner.rs`
- Modify: `crates/threadlane/src/components/panel_surface.rs`
- Modify: `crates/threadlane/src/components/section_header.rs`
- Modify: `crates/threadlane/src/components/session_row.rs`
- Modify: `crates/threadlane/src/components/status_dot.rs`
- Modify: `crates/threadlane/src/components/status_pill.rs`
- Modify: `crates/threadlane/src/components/task_sidebar.rs`
- Modify: `crates/threadlane/src/components/terminal_panel.rs`
- Modify: `crates/threadlane/src/components/tool_section.rs`
- Modify: `crates/threadlane/src/panels/sessions/view.rs`

**Interfaces:**
- Consumes the semantic tokens from Task 1.
- Produces UI script definitions with no hard-coded color literals.

- [x] Replace each literal with the matching semantic token, preserving all `color`, `color_hover`, `color_focus`, `color_down`, border, icon, text, and shader colors.
- [x] Preserve component-specific differences when two nearby literals are not exact matches.
- [x] Keep custom shader `clear(...)` colors explicit through a transparent theme token where script scope supports it.
- [x] Re-scan these files and leave only `theme.*` color references.

### Task 3: Migrate app shell UI and Rust-side draw colors

**Files:**
- Modify: `crates/threadlane/src/app/mod.rs`

**Interfaces:**
- Consumes the semantic tokens from Task 1.
- Keeps existing runtime layout and dynamic draw behavior unchanged.

- [x] Replace all `script_mod!` color literals in the app shell with semantic theme tokens.
- [x] Inspect Rust-created `Vec4f`/draw values separately; no hex UI literals required migration there.
- [x] Leave non-UI serialization, image encoding, or data-format colors untouched if any remain.
- [x] Re-scan `app/mod.rs` and confirm no UI color literal remains outside the theme module.

### Task 4: Document and verify the consolidation

**Files:**
- Modify: `AGENTS.md`

**Interfaces:**
- Documents the repository rule for future UI changes.

- [x] Add a concise theme rule: all UI colors must use `theme.*`; new literals belong only in `crates/threadlane/src/theme/mod.rs`; preserve explicit interaction-state colors and alpha.
- [x] Run the literal scan and verify matches are confined to `theme/mod.rs`.
- [x] Run `cargo check -p threadlane`.
- [x] Run `git diff --check`.
- [x] Review the final diff for accidental palette changes; runtime visual verification remains required for rendered UI changes.
