use gpui::*;
use gpui_component::Root;
use threadlane_gpui::assets::Assets;
use threadlane_gpui::screens::workspace::WorkspaceView;
use threadlane_gpui::theme;

fn main() {
    if std::env::args().any(|arg| arg == "--dump-config") {
        println!("active_lane=main");
        println!("session_file=<resolved per workspace>");
        println!("model=<selected model>");
        println!("provider=selected model router");
        println!("skills=project-and-global discovery");
        println!("extensions=global-and-project WASI modules");
        println!("sandbox=workspace-scoped capabilities");
        return;
    }
    env_logger::init();

    let app = gpui_platform::application().with_assets(Assets);

    app.run(move |cx| {
        gpui_component::init(cx);
        threadlane_gpui::screens::chat::init(cx);
        threadlane_gpui::screens::workspace::init(cx);
        theme::init(cx);

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
