//! Per-file diff overlay: colored unified-diff lines, opened by clicking a
//! changed file in the git panel. Read-only review v1 — staging/side-by-side
//! are on the roadmap.

use crate::diff::DiffLineKind;
use crate::views::root::RootView;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{h_flex, v_flex, ActiveTheme, IconName, Sizable as _};

pub fn render(view: &RootView, cx: &mut Context<RootView>) -> impl IntoElement {
    let theme = cx.theme();
    let (path, lines, loading) = match &view.diff_view {
        Some(dv) => (dv.path.clone(), dv.lines.clone(), dv.loading),
        None => (String::new(), Vec::new(), false),
    };

    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui::black().opacity(0.5))
        .id("diff-backdrop")
        .on_click(cx.listener(|this, _, _, cx| this.close_diff_view(cx)))
        .child(
            v_flex()
                .id("diff-card")
                .w(px(860.))
                .max_h(px(720.))
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .shadow_lg()
                .overflow_hidden()
                .on_click(cx.listener(|_, _, _, _| {}))
                .child(
                    h_flex()
                        .px_4()
                        .py_3()
                        .items_center()
                        .justify_between()
                        .border_b_1()
                        .border_color(theme.border)
                        .child(
                            div().text_sm().font_weight(FontWeight::BOLD).truncate().child(path),
                        )
                        .child(
                            Button::new("close-diff")
                                .icon(IconName::Close)
                                .ghost()
                                .small()
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.close_diff_view(cx)),
                                ),
                        ),
                )
                .child(if loading {
                    div()
                        .p_4()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("loading diff…")
                        .into_any_element()
                } else {
                    v_flex()
                        .id("diff-lines")
                        .p_3()
                        .max_h(px(640.))
                        .overflow_y_scroll()
                        .font_family("monospace")
                        .text_xs()
                        .children(lines.into_iter().map(|l| {
                            let (fg, bg) = match l.kind {
                                DiffLineKind::Addition => {
                                    (theme.success, Some(theme.success.opacity(0.08)))
                                }
                                DiffLineKind::Deletion => {
                                    (theme.danger, Some(theme.danger.opacity(0.08)))
                                }
                                DiffLineKind::HunkHeader => (theme.primary, None),
                                DiffLineKind::FileHeader => (theme.muted_foreground, None),
                                DiffLineKind::Context => (theme.foreground, None),
                            };
                            let text = if l.text.is_empty() { " ".to_string() } else { l.text };
                            div()
                                .px_1()
                                .text_color(fg)
                                .when_some_bg(bg)
                                .child(text)
                        }))
                        .into_any_element()
                }),
        )
}

/// Tiny helper: apply an optional background.
trait WhenSomeBg {
    fn when_some_bg(self, bg: Option<Hsla>) -> Self;
}

impl WhenSomeBg for Div {
    fn when_some_bg(self, bg: Option<Hsla>) -> Self {
        match bg {
            Some(color) => self.bg(color),
            None => self,
        }
    }
}
