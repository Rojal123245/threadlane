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
            color_background: #x09090b
            color_foreground: #xfafafa
            color_card: #x18181b
            color_card_foreground: #xfafafa
            color_popover: #x18181b
            color_popover_foreground: #xfafafa
            color_primary: #xfafafa
            color_primary_foreground: #x09090b
            color_secondary: #x27272a
            color_secondary_foreground: #xfafafa
            color_muted: #x27272a
            color_muted_foreground: #xa1a1aa
            color_accent: #x27272a
            color_accent_foreground: #xfafafa
            color_destructive: #xef4444
            color_destructive_foreground: #xfafafa
            color_success: #x22c55e
            color_success_foreground: #xfafafa
            color_warning: #xf59e0b
            color_warning_foreground: #xfafafa
            color_border: #x27272a
            color_input: #x27272a
            color_ring: #xa1a1aa
            color_transparent: #x00000000
            color_primary_tint: #xfafafa15
            color_success_tint: #x22c55e15
            color_destructive_tint: #xef444415
            color_accent_tint: #x27272a40
            color_state_hover: #x27272a
            color_state_active: #x3f3f46
            color_state_pressed: #x18181b

            // ── Radii ─────────────────────────────────────────────────
            radius_xs: 4.0
            radius_sm: 6.0
            radius_md: 8.0
            radius_lg: 10.0
            radius_xl: 12.0
            radius_2xl: 16.0
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
