# Black/White Logo Assets Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace every gradient Threadlane logo asset with the existing black/white sidebar mark.

**Architecture:** Keep `crates/threadlane/resources/icons/logo.svg` as the single logo source. Replace the documentation SVG with a black background and the same white mark, then regenerate the six packaged PNG sizes from that source for the packager metadata in `crates/threadlane/Cargo.toml`.

**Tech Stack:** SVG, PNG raster assets, ImageMagick or an installed native rasterizer, Cargo/Makepad asset references.

## Global Constraints

- Do not change Makepad UI code; existing UI references already use `resources/icons/logo.svg`.
- Do not add dependencies; use an installed image conversion tool.
- Preserve the six existing packaged icon filenames and dimensions.
- Verify visual output and `git diff --check` before completion.

---

### Task 1: Replace gradient branding assets

**Files:**
- Modify: `docs/images/threadlane-logo.svg`
- Modify: `resources/icon_32.png`
- Modify: `resources/icon_48.png`
- Modify: `resources/icon_64.png`
- Modify: `resources/icon_128.png`
- Modify: `resources/icon_256.png`
- Modify: `resources/icon_512.png`

**Interfaces:**
- Consumes: `crates/threadlane/resources/icons/logo.svg`.
- Produces: one black/white documentation logo and six same-brand packaged icons at their existing dimensions.

- [x] **Step 1: Replace the documentation SVG**

  Use a black `512x512` rounded-square background and embed the existing sidebar mark as white stroked paths. Remove all gradients, glow filters, animation, and colored stops.

- [x] **Step 2: Regenerate packaged PNGs**

  Render the simplified SVG to `32`, `48`, `64`, `128`, `256`, and `512` pixel PNGs, preserving the existing filenames under `resources/`.

- [x] **Step 3: Inspect the generated assets**

  Confirm the files remain PNGs with dimensions matching their filenames and visually contain only the black background plus the white mark.

- [x] **Step 4: Verify repository references and whitespace**

  Run `rg -n -i "linearGradient|radialGradient|34D399|6366F1|7C3AED" docs/images resources crates/threadlane/resources` and `git diff --check`. The gradient search should return no logo-gradient matches, and the whitespace check should exit successfully.

- [x] **Step 5: Run the focused build check**

  Run `cargo check -p threadlane` to ensure the existing Makepad asset references and packaging metadata remain valid.
