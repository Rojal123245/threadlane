# Higher-Contrast Activity Loader Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every shared dot-grid loading indicator clearly visible with a cyan/blue/violet palette and fully opaque dot states.

**Architecture:** Keep the existing shared `ActivityLoader` shader and change only its four palette uniforms and alpha calculation. Existing consumers inherit the new appearance without changes to component geometry, speed, animation path, or per-instance overrides.

**Tech Stack:** Rust, Makepad component script, Makepad pixel shader DSL

## Global Constraints

- Change only loader colors and opacity behavior.
- Preserve grid geometry, dimensions, speed defaults, animation timing, and per-instance overrides.
- Keep geometric edge antialiasing through the existing `coverage` value.
- Add no dependencies or new components.

---

### Task 1: Increase Shared Activity Loader Contrast

**Files:**
- Modify: `crates/threadlane/src/components/activity_loader.rs:19-22,56-62`
- Test: no persistent automated test; this is a shader palette-only change validated through a focused source acceptance check, Makepad compilation, and runtime visual inspection

**Interfaces:**
- Consumes: the existing `mod.components.ActivityLoader` component and its `draw_bg` shader uniforms
- Produces: the same `mod.components.ActivityLoader` interface with a brighter shared appearance

- [ ] **Step 1: Verify the intended appearance is absent**

Run:

```bash
! grep -qF 'color: uniform(#x66d9ff)' crates/threadlane/src/components/activity_loader.rs && \
! grep -qE '^[[:space:]]*let alpha = coverage[[:space:]]*$' crates/threadlane/src/components/activity_loader.rs
```

Expected: exit status 0, confirming the new head color and full-opacity calculation are not present yet.

- [ ] **Step 2: Apply the minimal shader change**

In `crates/threadlane/src/components/activity_loader.rs`, replace the palette uniforms with:

```text
color: uniform(#x66d9ff)
color_mid: uniform(#x6fa8ff)
color_tail: uniform(#xa78bfa)
color_idle: uniform(#x7067d9)
```

Replace:

```text
let alpha = coverage * (0.42 + head * 0.58 + trail * 0.34)
```

with:

```text
let alpha = coverage
```

Do not alter any other shader expression or component property.

- [ ] **Step 3: Verify the source acceptance conditions**

Run:

```bash
grep -qF 'color: uniform(#x66d9ff)' crates/threadlane/src/components/activity_loader.rs && \
grep -qF 'color_mid: uniform(#x6fa8ff)' crates/threadlane/src/components/activity_loader.rs && \
grep -qF 'color_tail: uniform(#xa78bfa)' crates/threadlane/src/components/activity_loader.rs && \
grep -qF 'color_idle: uniform(#x7067d9)' crates/threadlane/src/components/activity_loader.rs && \
grep -qE '^[[:space:]]*let alpha = coverage[[:space:]]*$' crates/threadlane/src/components/activity_loader.rs
```

Expected: exit status 0.

- [ ] **Step 4: Validate Makepad and Rust compilation**

Run:

```bash
cargo check -p threadlane
```

Expected: exit status 0. Existing unrelated warnings are acceptable.

- [ ] **Step 5: Validate and review the patch**

Run:

```bash
git diff --check && git diff -- crates/threadlane/src/components/activity_loader.rs
```

Expected: exit status 0 from `git diff --check`; the displayed component diff contains only four color replacements and the alpha-expression replacement.

- [ ] **Step 6: Commit the implementation**

```bash
git add crates/threadlane/src/components/activity_loader.rs
git commit -m "style: increase activity loader contrast"
```

- [ ] **Step 7: Perform runtime visual verification**

Run:

```bash
cargo run -p threadlane
```

Start an agent response and confirm that the moving cyan/blue/violet color sequence is clearly distinguishable, all inactive dots remain visible, and the loader retains its original size and motion. This step requires direct observation and must not be reported as complete unless the app is actually viewed.
