//! Session sidebar: project name, session list with running indicators,
//! new-session button, provider + settings footer.

use crate::views::root::RootView;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{h_flex, v_flex, ActiveTheme, Icon, IconName, Sizable as _};

/// "950", "12.4k", "3.1M" — sidebar-compact token counts.
pub fn format_tokens(n: u64) -> String {
    match n {
        0..=999 => format!("{n} tok"),
        1_000..=999_999 => format!("{:.1}k tok", n as f64 / 1_000.),
        _ => format!("{:.1}M tok", n as f64 / 1_000_000.),
    }
}

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
    let provider_label = view.workspace.settings.active_profile().label();

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
                        .child(div().text_sm().font_weight(FontWeight::BOLD).child(project_name)),
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
            v_flex()
                .flex_1()
                .px_2()
                .gap_1()
                .overflow_hidden()
                .child(
                    div()
                        .px_2()
                        .pt_1()
                        .text_xs()
                        .font_weight(FontWeight::BOLD)
                        .text_color(theme.muted_foreground)
                        .child("SESSIONS"),
                )
                .children(rows.into_iter().map(|row| {
                    let i = row.index;
                    let is_active = active == Some(i);
                    let meta = if row.total_tokens > 0 {
                        format!("{} · {}", row.provider_label, format_tokens(row.total_tokens))
                    } else {
                        row.provider_label
                    };
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
                                        .bg(if row.running { theme.success } else { theme.muted }),
                                )
                                .child(div().text_sm().flex_1().truncate().child(row.title))
                                .child(
                                    // Close the session; its worktree and
                                    // branch stay (cleaned up from the git
                                    // panel when wanted).
                                    Button::new(("close-session", i))
                                        .icon(IconName::Close)
                                        .ghost()
                                        .xsmall()
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.close_session_row(i, cx)
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .pl_3()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .truncate()
                                .child(meta),
                        )
                })),
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
