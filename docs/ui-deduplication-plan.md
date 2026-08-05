# UI Deduplication Plan

Reduce code duplication in the Threadlane UI by reusing existing components and
extracting generic, parameterized components. Goal: measurably fewer duplicate
DSL blocks and near-identical Rust handlers, with **zero visual or behavioral
change**.

Status: completed (Phases 1 through 5 successfully executed and verified).

---

## 1. Evidence inventory (what is duplicated, and where)

All line numbers refer to the current `main` state of `crates/threadlane/src`.

### 1.1 DSL duplication in `app/mod.rs` (`script_mod!`, lines 311–3224)

| # | Duplication | Location(s) | Size / similarity |
|---|---|---|---|
| F1 | Four fold-header chat message templates sharing an identical 14-line prefix, near-identical `summary` block (5 copies), and identical transparent `body` style block | `ActivityGroupMsg` 491–531, `ThinkingMsg` 533–578, `SubagentMsg` 580–713 (incl. nested `row_template` 630–712), `ToolMsg` 715–794 | ~250–300 dup lines; skeleton ~90% identical |
| F2 | "Add server/agent" card copy-pasted | `mcp_add_card` 1520–1604 vs `acp_add_card` 1689–1773 | ~150 dup lines; ~85% identical (only strings + IDs differ) |
| F3 | Four settings-page scaffolds (header row + description + `PortalList` + status label) | `capabilities_page` 1323–1413, `skills_page` 1415–1471, `mcp_page` 1473–1629, `acp_page` 1631–1798; identical `status_lbl` block 4× (1403–1412, 1461–1470, 1619–1628, 1788–1797) | ~120–150 dup lines |
| F4 | Three provider pages with repeated `ProviderCard` scaffolding; identical secondary-button style block 2× | `google_antigravity_page` 1107–1192, `openai_page` 1194–1246, `opencode_page` 1248–1321 | ~120 dup lines |
| F5 | Four right-sidebar tab `IconButton`s differing only in SVG | 3085–3144 | ~56 dup lines |
| F6 | Six toolbar `IconButton` instances with the same shape | 1980–1995, 2076–2084, 2130–2145, 3027–3057 | ~60 dup lines |
| F7 | `UserMsg` vs `UserMsgWrapped` — ~95% identical (only Fit vs Fill width strategy differs) | 429–446 vs 448–463 | ~18 dup lines, but on the hot streaming path |
| F8 | Style block `draw_bg +: { color: theme.color_transparent, border_size: 0.0 }` | 521–528, 568–575, 614–623, 683–707, 764–773 | 5 identical copies (~30 lines) |
| F9 | Identical transparent `ScrollBar` block | ChatList + SessionList `PortalList`s | 2 copies (~30 lines) |
| F10 | 8 trivial `NavButton` instances | 1016–1051 | 8 × 4 lines |
| F11 | 4 `StarterPromptCard` instances differing only in tint/icon/text | 385–414 | 4 × 7 lines |
| F12 | Global/Project scope-button pairs | 1354–1367, 1494–1507, 1657–1670 | 3 identical pairs |
| F13 | Repeated `TextInput` style blocks (input bg + focus border) | 1550–1586, 1719–1755, 2004–2022, 3178–3191 | ~90 dup lines, 5 copies |
| F14 | Empty-row blocks | 954–960 + 4× `CapabilityEmptyRow` | ~25 dup lines |
| F15 | Popover `draw_bg` style blocks | 832–837, 2541–2546, 3079–3084, 3162–3167 | 4 near-identical |
| F16 | Status-dot rows | 2170–2178 vs 2683–2686 | 2 copies |

### 1.2 Rust duplication in `app/mod.rs` (lines 3228–9498)

| # | Duplication | Location(s) | Size / similarity |
|---|---|---|---|
| R1 | MCP vs ACP capability CRUD handlers — same "get row → load configs → mutate → save → refresh → status" skeleton | `set_mcp_enabled` 5201–5235 / `remove_mcp_server` 5237–5269 / `add_mcp_server` 5271–5388 vs `set_acp_enabled` 5478–5505 / `remove_acp_agent` 5507–5529 / `add_acp_agent` 5531–5574; same shape also in `set_skill_enabled` 5586–5609, `set_extension_enabled`/`remove_extension`/`install_extension` 5716–5808 | ~200 dup lines; ~65–70% identical |
| R2 | `set_*_status` modal-borrow wrappers | `set_acp_status` 5390–5398, `set_skill_status` 5576–5584, `set_capability_status` 5655–5663 (+ inlined borrows at 5281–5288, 5370–5385) | 4 identical 8-line bodies |
| R3 | `refresh_*_state` parallel refreshers (spawn → discover → send event → signal) | `refresh_capability_state` 5147–5159, `refresh_skill_state` 5161–5172, `refresh_mcp_state` 5174–5186, `refresh_acp_state` 5441–5476 | ~60–70% identical |
| R4 | Three parallel provider branches in one function | `refresh_provider_connection_ui` 5810–5890 | ~80 lines, near-identical shape |
| R5 | Mechanical `if self.ui.button(cx, ids!(X)).clicked(actions)` chains | `handle_action` 4030–4460 (tab buttons 4310–4324, attachment chips 4636–4649, login buttons 4034–4082) | ~400 lines of repeated idiom |
| R6 | Dropdown-rebuild idiom (labels → `set_labels` → `set_selected_item` → `set_visible`) | `sync_git_branch_picker` 6674–6693, `set_model_dropup_options` 6133–6160 | same shape |
| R7 | One-line git passthroughs | `start_git_push` 7320–7322, `start_git_pull` 7324–7326 | identical |
| R8 | Context-menu open/close idiom repeated across sites | 4603–4630, 7817–7823 | 2 sites |

### 1.3 Component-level duplication (`components/*.rs`, 43 files)

| # | Candidate group | Files | Verdict |
|---|---|---|---|
| C1 | **Overlay popup mechanics** — `draw_list: Option<DrawList2d>` + `ScriptHook::on_after_new` + turtle draw + edge-clamp + outside-click/Escape dismissal | `context_menu.rs`, `context_window.rs`, `provider_settings_modal.rs`, `model_dropdown.rs` (+ `command_input.rs` popup) | ~80% of the Rust is the same skeleton; highest ROI |
| C2 | **Buttons** — same transparent/accent state matrix | `icon_button.rs`, `nav_button.rs`, `composer_action.rs`, `header_chip.rs` + standalone dupes `TerminalIconButton` (terminal_panel.rs:40–48), `TerminalTabCloseButton` (29–38), `ContextMenuItem` (context_menu.rs:154–179), file-tree `Node` (file_tree.rs:93–113), `path_btn` (git_changes.rs:121–141) | one parameterized `ThemedButton` would collapse ~9 definitions |
| C3 | **Labels** — `Label{width, color, font_size, overflow}` variants | `clipped_label.rs`, `code_label.rs` + primitives in `page_header.rs:8–51`, `section_header.rs:8–16`, `provider_card.rs:29–54`, `git_diff.rs:28–52` | one `ThemedLabel` primitive covers ~10 call sites |
| C4 | **List rows** — `RoundedView card + status dot + text column + trailing action` | `session_row.rs`, `capability_row.rs`, `auth_row.rs`, `starter_prompt_card.rs`, `empty_row.rs`, task_sidebar rows (task_sidebar.rs:15–133), git_changes file row | one `ListRow` base; `SessionRow`'s tree-line shader stays unique |
| C5 | **Status dots** — 3×3/5×5/7×7 colored dot re-declared | `status_dot.rs` (exists!), `status_pill.rs`, `activity_loader.rs`, **inline dupes** task_sidebar.rs:32–39, 115–122 | replace inline dupes with existing `StatusDot` |
| C6 | **Headers** — trivial containers | `panel_header.rs`, `section_header.rs`, `page_header.rs` | parameterizable; `activity_header.rs` + `project_header.rs` are ~85% unique, leave alone |

### 1.4 Cross-cutting Rust idioms (components)

- P3: `let Event::Actions(actions) = event else { return }` + `.clicked(actions)` dispatch — 9 files.
- P7: PortalList draw/fill loop with empty-row fallback — 6 files.
- P8: `list.items_with_actions(actions)` dispatch — 4 near-identical blocks in `provider_settings_modal.rs:304–348`.
- P2: hover-state bool + child visibility toggle + `redraw` — 4 widgets.
- P5: `NextFrame` animation loops — 3 widgets.

**Already-good examples to copy:** `sidebar_compose_button.rs` (inherits `IconButton`),
`CapabilityRowWithRemove = CapabilityRowBase` (DSL override pattern). The convention
exists; it is just not applied consistently.

---

## 2. Guiding principles

1. **Behavior-neutral.** Every extraction preserves pixel-identical output and
   identical interaction. Visual verification after each phase (see §6).
2. **Components own their children.** When a widget's nested controls are read or
   clicked from the app shell, move that interaction into the component and emit a
   typed `cx.widget_action(...)` (AGENTS.md component conventions) instead of
   root-level `ids!(...)` lookups into a template's guts.
3. **One generic per family, thin variants.** Define a `=` template in
   `components/` with parameterized slots; instantiations override only what they
   need. `:=` ID-bound instances cannot be prototype parents — only `=`
   `mod.components.*` names can (AGENTS.md DSL inheritance rule).
4. **Reuse before extract.** First replace inline copies with an existing
   component (e.g. inline dots → `StatusDot`); only extract a new generic when
   ≥3 call sites share a stable shape.
5. **Don't touch hot paths.** The chat streaming draw path
   (`ChatList::draw_walk`, `draw_markdown_item`) and `SubagentRail` have
   performance-sensitive manual draw loops; consolidation there must not add
   per-frame work, clones, or re-parses.
6. **Registration discipline.** Every new component module must be added to both
   the Rust `mod` list and the `script_mod(vm)` sequence in
   `components/mod.rs`, and initialized in `components/init.rs`.
7. **Theme tokens only.** Extracted components must keep using `theme.*` role
   tokens; no new color literals outside `theme/mod.rs`.

---

## 3. Generic components to create

| New component | Based on | Parameters (DSL overrides) | Replaces |
|---|---|---|---|
| `SecondaryActionButton` | `SettingsActionButton` style | text, icon | F4 duplicate style blocks (1165–1189, 1300–1318), F12 scope pairs |
| `ThemedTextInput` | mcp/acp/git-dialog inputs | placeholder, width, radius | F13 (5 copies) |
| `ChatFoldRowBase` | F1 common skeleton | header icon/title, body padding, `body_slot` child | F1's four templates become thin `=` overrides |
| `AddServerCard` | F2 | title, name placeholder, command placeholder, submit text | F2 (mcp + acp add cards) |
| `SettingsPage` | F3 | title, description, list row template, empty text | F3's four page scaffolds |
| `ThemedButton` | C2 matrix | size, padding, icon_walk, bg/border/text state colors, selected-animator flag | C2's ~9 button definitions |
| `ThemedLabel` | C3 | width mode, color, font, size, overflow | C3's ~10 label primitives |
| `ListRow` | C4 | height, padding, spacing, bg, optional status dot, optional trailing slot | C4 row family |
| `OverlayPopup` (Rust base) | C1 | content view, open/close, clamp geometry, dismissal keys | C1's four widgets' shared skeleton |

Deliberately **not** consolidated: `ActivityHeader`/`ProjectHeader` (mostly unique),
`ActivityLoader` shader, `SessionTitle` marquee, `ContextWindow` tooltip drawing
(unique content; only the skeleton is shared).

---

## 4. Phased execution

Order = ROI / risk. Each phase ends green (`cargo check`, focused tests, diff
review) before the next starts.

### Phase 1 — Micro-consolidations (low risk, no structural change)
1. Replace inline 7×7 dot rows with existing `StatusDot` (task_sidebar.rs:32–39, 115–122).
2. Extract `SecondaryActionButton` from the repeated style block (F4) and use it
   for the doctor/clear buttons and the Global/Project scope pairs (F12).
3. Extract `ThemedTextInput` (F13); swap in mcp/acp inputs and git-branch dialog.
4. Merge `UserMsg`/`UserMsgWrapped` into one template with a `wrap`-flavored
   override — **careful**: preserve the Fit/Fill width distinction and the
   `md +: { width: Fill }` rule; run the app and verify wrapping behavior.
5. Replace the 4 repeated `status_lbl` blocks and empty rows with `ThemedLabel`
   / `EmptyRowBase` usage (F3/F14).

Expected: −300–400 lines of DSL; zero behavioral surface. Risk: minimal.

### Phase 2 — Chat activity-row extraction (medium risk, high line count)
1. Create `ChatFoldRowBase` in `components/` capturing the common prefix,
   `summary` block, transparent body style, and a `body_slot`.
2. Rewrite `ActivityGroupMsg`, `ThinkingMsg`, `SubagentMsg`, `ToolMsg` (F1) as
   thin `=` overrides. Keep the `SubagentRail` row template and `ToolMsg`'s extra
   labels as overrides, not new copies.
3. Preserve all invariants from AGENTS.md chat sections: grouping is
   presentation-only, streaming merges into the trailing group, `ToolFoldHeader`
   `LayoutChanged` handling, `draw_all_unscoped` consumption in `SubagentRail`.

Expected: −200–250 lines. Risk: medium — verify streaming, folding, and the
rail manually; `cargo check` cannot catch DSL inheritance mistakes.

### Phase 3 — Settings modal consolidation (medium risk, high line count)
1. Extract `AddServerCard` (F2) and `SettingsPage` (F3) as components that own
   their inner controls (inputs, scope buttons, refresh, portal list, status
   label) and emit typed actions.
2. Move the four near-identical `items_with_actions` dispatch blocks
   (provider_settings_modal.rs:304–348) into the components.
3. Update `sync_page_visibility` / page-branch logic in
   `ProviderSettingsModal::draw_walk` to the new component tree — keep the
   two-PortalList `widget_uid()` branch discipline from AGENTS.md.
4. Rust side (parallel track): collapse R1 (generic capability CRUD helper over
   MCP/ACP/skills/extensions configs), R2 (one `set_status` helper), R3 (one
   refresh helper with a manager-agnostic closure/event parameter), R4
   (provider-branch table). Keep persistence semantics and status strings
   byte-identical.

Expected: −350–500 lines total. Risk: medium — settings modal is dense; do
DSL and Rust in separate commits within the phase.

### Phase 4 — Component-library generics (lower urgency, deliberate)
1. `ThemedButton` (C2): parameterize the existing state matrix; convert
   `NavButton`, `HeaderChipButton`, `ComposerChip`, `TerminalIconButton`,
   `ContextMenuItem`, file-tree/git row buttons to overrides. Keep the
   `selected` animator convention.
2. `ThemedLabel` (C3): convert label primitives across page/section/provider
   card/git diff.
3. `ListRow` (C4): only where ≥3 rows share the shape; `SessionRow` keeps its
   shader and state logic.
4. `OverlayPopup` Rust base (C1): extract the `DrawList2d` field + hook +
   turtle draw + clamp + dismissal into a reusable base or helper trait;
   convert `context_menu`, `context_window`, `provider_settings_modal`,
   `model_dropdown` to use it. Do **not** change popup geometry constants
   (transparent-anchor rule for dropdowns, menu height/edge gaps).
5. Header trio (`panel/section/page_header`) — only if it falls out naturally
   from `ThemedLabel`; otherwise skip (low ROI).

Expected: −400–600 lines. Risk: medium-high; requires running the app for
visual verification of every converted control.

### Phase 5 — Verification, measurement, documentation
1. Full workspace checks: `cargo check -p threadlane`, `cargo test --workspace`,
   `git diff --check`.
2. Manual visual pass of: chat streaming/folding, composer dropdowns, settings
   modal (all pages, scope toggles, add-card), sidebar hover states, context
   menu, terminal tabs.
3. Measure: line-count delta per file, duplicate-block count (grep for the F/R
   markers), `THREADLANE_PERF=1` on a release build to confirm no UI regression
   (especially chat streaming).
4. Update this plan to done/notes; record durable conventions in `AGENTS.md`
   (e.g. "new buttons derive from `ThemedButton`", "overlays derive from
   `OverlayPopup`") and `.threadlane/memory.md`.

---

## 5. Explicitly out of scope

- Behavior, layout, or theme changes (including popup geometry).
- Consolidating panels' single-widget implementations (chat/sessions views are
  already monolithic, not duplicated).
- Splitting `app/mod.rs` into more files without a duplication win.
- The mechanical `clicked()` chains (R5) as a macro: risky for readability;
  revisit only if R1–R4 land cleanly.
- Optimizing `cached_base_rows.clone()` or other streaming-path perf items
  (tracked separately in memory.md performance findings).

## 6. Definition of done

- Zero behavior change: every screen renders and interacts identically
  (checked per phase by running `cargo run -p threadlane`).
- No component registered without both `mod` list + `script_mod` entry in
  `components/mod.rs`.
- No new color literals outside `theme/mod.rs`; icon-only buttons keep
  `padding: 0, spacing: 0, text: ""`.
- Net reduction: ≥1,000 lines of duplication across `app/mod.rs`,
  `components/*`, and settings modal; every F/R/C item above either resolved
  or explicitly rejected with a reason.
- All validation commands from AGENTS.md pass, plus a release-build perf spot
  check on chat streaming.
