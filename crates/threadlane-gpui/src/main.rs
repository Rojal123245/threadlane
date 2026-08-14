mod app;
mod chat_list;
mod sidebar;
mod state;

use app::WorkspaceView;
use gpui::*;
use gpui_component::Root;

fn main() {
    env_logger::init();

    let app = gpui_platform::application();

    app.run(move |cx| {
        gpui_component::init(cx);

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(1100.0), px(720.0)),
                cx,
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some("Threadlane (GPUI)".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(12.0), px(12.0))),
            }),
            ..Default::default()
        };

        cx.spawn(async move |cx| {
            cx.open_window(options, |window, cx| {
                let view = WorkspaceView::build(window, cx);
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("failed to open GPUI window");
        })
        .detach();
    });
}
