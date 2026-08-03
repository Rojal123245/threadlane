//! Page header title and description label primitives.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.PageTitleLabel = Label {
        width: Fill
        height: Fit
        draw_text +: {
            color: theme.color_foreground
            text_style: theme.font_bold { font_size: 16.0 }
        }
    }

    mod.components.PageDescriptionLabel = Label {
        width: Fill
        height: Fit
        draw_text +: {
            color: theme.color_muted_foreground
            text_style +: { font_size: 10.0 }
        }
    }

    mod.components.CategoryHeaderLabel = Label {
        width: Fill
        height: Fit
        draw_text +: {
            color: theme.color_muted_foreground
            text_style: theme.font_bold { font_size: 9.0 }
        }
    }

    mod.components.HeadlineLabel = Label {
        width: Fit
        height: Fit
        draw_text +: {
            color: theme.color_card_foreground
            text_style: theme.font_bold { font_size: 20.0 }
        }
    }

    mod.components.HeadlineAccentLabel = Label {
        width: Fit
        height: Fit
        draw_text +: {
            color: theme.color_foreground
            text_style: theme.font_bold { font_size: 20.0 }
        }
    }

    mod.components.PageHeader = View {
        width: Fill
        height: Fit
        flow: Down
        spacing: 4
        title := mod.components.PageTitleLabel {}
        description := mod.components.PageDescriptionLabel {}
    }
}
