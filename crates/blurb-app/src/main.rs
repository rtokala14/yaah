//! blurb — desktop agentic coding harness.
//!
//! Bootstrap follows gpui-component's canonical shape: platform application,
//! `gpui_component::init`, window opened from a spawned task, root view
//! wrapped in `gpui_component::Root`.

mod settings;
mod transcript;
mod views;
mod workspace;

use gpui::*;
use gpui_component::{ActiveTheme, Root};
use settings::Settings;
use std::path::PathBuf;
use views::root::RootView;

fn main() {
    let project_root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let project_root = project_root.canonicalize().unwrap_or(project_root);

    gpui_platform::application()
        .with_assets(gpui_component_assets::Assets)
        .run(move |cx: &mut App| {
            gpui_component::init(cx);

            let settings = Settings::load();
            let options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("blurb".into()),
                    ..Default::default()
                }),
                window_min_size: Some(size(px(900.), px(600.))),
                ..Default::default()
            };
            cx.spawn(async move |cx| {
                cx.open_window(options, move |window, cx| {
                    let view = cx.new(|cx| RootView::new(project_root, settings, window, cx));
                    cx.new(|cx| Root::new(view, window, cx).bg(cx.theme().background))
                })
                .expect("failed to open window");
                cx.update(|cx| cx.activate(true)).ok();
            })
            .detach();
        });
}
