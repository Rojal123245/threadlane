//! Threadlane theme tokens.
//!
//! Extends Makepad's built-in `mod.theme` with a single, project-wide set of
//! semantic color and dimension tokens so that every panel and component can
//! source its visuals from `theme.*` instead of hard-coded `#xRRGGBB` literals.
//!
//! The tokens are installed once, very early in `AppMain::script_mod`, before
//! any `components::script_mod` runs. Components and panels then reference them
//! as `theme.color_background`, `theme.color_foreground`, etc.
//!
//! Token naming conventions:
//! - `color_bg_*`     — background surfaces, ordered from app shell inward.
//! - `color_border_*` — border / divider colors.
//! - `color_text_*`   — foreground text, ordered by prominence.
//! - `color_accent_*` — brand / status accents (blue, green, red, purple, orange).
//! - `color_state_*`  — interactive surface states (hover, focus, down, active).
//! - `radius_*`       — corner radii.
//! - `space_*`        — spacing scale.

use makepad_widgets::*;

pub fn install(vm: &mut ScriptVm) {
    script_eval!(vm, {
        use mod.math.*
        use mod.res.*

        mod.theme = mod.theme {
            // Semantic roles: components should consume these roles, not
            // individual palette shades.
            color_background: #x181a1f
            color_foreground: #xe7ebf0
            color_card: #x1f232b
            color_card_foreground: #xd0d8e4
            color_popover: #x1b232dc8
            color_popover_foreground: #xe7ebf0
            color_primary: #x6fa8ff
            color_primary_foreground: #xffffff
            color_secondary: #x232830
            color_secondary_foreground: #xd0d8e4
            color_muted: #x232830
            color_muted_foreground: #x9ba7b6
            color_accent: #x2d3744
            color_accent_foreground: #xe7ebf0
            color_destructive: #xe5534b
            color_destructive_foreground: #xffffff
            color_success: #x67c58b
            color_success_foreground: #xffffff
            color_warning: #xd2a85d
            color_warning_foreground: #xffffff
            color_border: #x3a424e
            color_input: #x2a3441
            color_ring: #x6fa8ff
            color_transparent: #x00000000
            color_primary_tint: #x5fa0de18
            color_success_tint: #x3aaa7818
            color_destructive_tint: #xc0703018
            color_accent_tint: #x9a75d518
            color_state_hover: #x303844
            color_state_active: #x354153
            color_state_pressed: #x2d37446b

            // ── Radii ─────────────────────────────────────────────────
            radius_xs: 5.0
            radius_sm: 6.0
            radius_md: 8.0
            radius_lg: 10.0
            radius_xl: 11.0
            radius_2xl: 14.0
            radius_pill: 9999.0

            // ── Spacing scale ────────────────────────────────────────
            space_1: 4.0
            space_2: 6.0
            space_3: 8.0
            space_4: 10.0
            space_5: 12.0
            space_6: 14.0
        }
    });
}
