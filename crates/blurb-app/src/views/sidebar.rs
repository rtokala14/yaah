//! Session sidebar: project name, session list with running indicators,
//! new-session button, provider + settings footer.

use crate::views::root::RootView;
use gpui::*;
use gpui_component::button::Button;
use gpui_component::{h_flex, v_flex, ActiveTheme, Icon, IconName};

pub fn render(view: &RootView, cx: &mut Context<RootView>) -> impl IntoElement {
    let theme = cx.theme();
    let project_name = view
        .workspace
        .project_root
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "project".into());
    let rows = view.workspace.session_rows();
    let active = view.workspace.active_session;
    let provider_label = view.workspace.settings.active().name.clone();

    v_flex()
        .size_full()
        .bg(theme.sidebar)
        .border_r_1()
        .border_color(theme.sidebar_border)
        // Header: project + new session
        .child(
            h_flex()
                .p_3()
                .gap_2()
                .items_center()
                .justify_between()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Icon::new(IconName::Folder).text_color(theme.muted_foreground))
                        .child(div().text_sm().font_bold().child(project_name)),
                )
                .child(
                    Button::new("new-session")
                        .icon(IconName::Plus)
                        .ghost()
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| this.new_session(cx))),
                ),
        )
        // Session list
        .child(
            v_flex().flex_1().px_2().gap_1().overflow_hidden().children(
                rows.into_iter().map(|(i, title, running, provider)| {
                    let is_active = active == Some(i);
                    div()
                        .id(("session", i))
                        .px_2()
                        .py_1p5()
                        .rounded(theme.radius)
                        .cursor_pointer()
                        .when(is_active, |d| d.bg(theme.sidebar_accent))
                        .hover(|d| d.bg(theme.sidebar_accent))
                        .on_click(cx.listener(move |this, _, _, cx| this.select_session(i, cx)))
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(
                                    div()
                                        .size_1p5()
                                        .rounded_full()
                                        .bg(if running { theme.success } else { theme.muted }),
                                )
                                .child(div().text_sm().flex_1().truncate().child(title)),
                        )
                        .child(
                            div()
                                .pl_3()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .truncate()
                                .child(provider),
                        )
                }),
            ),
        )
        // Footer: provider + settings
        .child(
            h_flex()
                .p_3()
                .gap_2()
                .items_center()
                .justify_between()
                .border_t_1()
                .border_color(theme.sidebar_border)
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .truncate()
                        .child(provider_label),
                )
                .child(
                    Button::new("settings")
                        .icon(IconName::Settings)
                        .ghost()
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_settings(cx))),
                ),
        )
}
