//! Shared provider settings card primitives.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.ProviderCard = RoundedView {
        width: Fill
        height: Fit
        flow: Down
        padding: Inset{left: 16 top: 14 right: 16 bottom: 14}
        spacing: 10
        draw_bg +: {
            color: theme.color_background
            border_radius: 8.0
            border_size: 1.0
            border_color: theme.color_card
        }
    }

    mod.components.ProviderCardHeader = View {
        width: Fill
        height: Fit
        flow: Right
        align: Align{y: 0.5}
    }

    mod.components.ProviderCardTitle = Label {
        width: Fill
        height: Fit
        draw_text +: {
            color: theme.color_foreground
            text_style: theme.font_bold { font_size: 11.5 }
        }
    }

    mod.components.ProviderCardStatus = Label {
        width: Fit
        height: Fit
        draw_text +: {
            color: theme.color_destructive
            text_style: theme.font_bold { font_size: 10.0 }
        }
    }

    mod.components.ProviderCardDescription = Label {
        width: Fill
        height: Fit
        draw_text +: {
            color: theme.color_muted_foreground
            text_style +: { font_size: 9.25 }
        }
    }

    mod.components.ProviderCardActions = View {
        width: Fill
        height: Fit
        flow: Right
        spacing: 8
        align: Align{y: 0.5}
    }
}
