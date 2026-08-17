//! Git panel: branch/HEAD, working-tree status, diffstat, worktrees (with
//! owning sessions), commit box. Renders from the immutable RepoSnapshot the
//! background refresher produces.

use crate::views::root::RootView;
use gpui::*;
use gpui_component::button::Button;
use gpui_component::input::Input;
use gpui_component::{h_flex, v_flex, ActiveTheme, Icon, IconName};

pub fn render(view: &RootView, cx: &mut Context<RootView>) -> impl IntoElement {
    let theme = cx.theme();
    let wt_sessions = view.workspace.worktree_sessions();

    let body: AnyElement = match (&view.workspace.git, &view.workspace.git_error) {
        (Some(snap), _) => {
            let branch = snap.head_branch.clone().unwrap_or_else(|| "(detached)".into());
            let statuses = snap.statuses.clone();
            let worktrees = snap.worktrees.clone();
            let diff = snap.diff.clone();

            v_flex()
                .flex_1()
                .gap_3()
                .p_3()
                .overflow_hidden()
                // HEAD
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(Icon::new(IconName::GitBranch).text_color(theme.muted_foreground))
                                .child(div().text_sm().font_bold().child(branch))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(snap.head_short_id.clone()),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .truncate()
                                .child(snap.head_summary.clone()),
                        ),
                )
                // Diffstat
                .child(
                    h_flex()
                        .gap_2()
                        .text_xs()
                        .child(div().child(format!("{} files", diff.files_changed)))
                        .child(div().text_color(theme.success).child(format!("+{}", diff.insertions)))
                        .child(div().text_color(theme.danger).child(format!("−{}", diff.deletions))),
                )
                // Status list
                .child(
                    v_flex().gap_0p5().flex_1().overflow_hidden().children(
                        statuses.into_iter().take(60).map(|s| {
                            h_flex()
                                .gap_2()
                                .text_xs()
                                .child(
                                    div()
                                        .w(px(20.))
                                        .text_color(status_color(&s.code, theme))
                                        .child(s.code.clone()),
                                )
                                .child(div().flex_1().truncate().child(s.path.clone()))
                        }),
                    ),
                )
                // Worktrees
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .font_bold()
                                .text_color(theme.muted_foreground)
                                .child("WORKTREES"),
                        )
                        .children(worktrees.into_iter().map(|wt| {
                            let session = wt_sessions.get(&wt.name).copied();
                            let name = wt.name.clone();
                            h_flex()
                                .gap_2()
                                .items_center()
                                .text_xs()
                                .child(div().flex_1().truncate().child(format!(
                                    "{} ⎇ {}",
                                    wt.name,
                                    wt.branch.clone().unwrap_or_default()
                                )))
                                .when_some(session, |this, ix| {
                                    this.child(
                                        div()
                                            .px_1p5()
                                            .rounded(theme.radius)
                                            .bg(theme.muted)
                                            .child(format!("session {}", ix + 1)),
                                    )
                                })
                                .child(
                                    Button::new(SharedString::from(format!("rm-wt-{name}")))
                                        .icon(IconName::Trash)
                                        .ghost()
                                        .xsmall()
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            // Close the owning session (if any) and
                                            // remove the worktree; branch is kept.
                                            let idx = this
                                                .workspace
                                                .worktree_sessions()
                                                .get(&name)
                                                .copied();
                                            if let Some(i) = idx {
                                                this.workspace.close_session(i, true);
                                            }
                                            cx.notify();
                                        })),
                                )
                        })),
                )
                .into_any_element()
        }
        (None, Some(err)) => v_flex()
            .p_3()
            .child(div().text_xs().text_color(theme.danger).child(err.clone()))
            .into_any_element(),
        (None, None) => v_flex()
            .p_3()
            .child(div().text_xs().text_color(theme.muted_foreground).child("loading git…"))
            .into_any_element(),
    };

    v_flex()
        .size_full()
        .border_l_1()
        .border_color(theme.border)
        .child(
            h_flex()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(theme.border)
                .child(div().text_sm().font_bold().child("Git")),
        )
        .child(body)
        // Commit box
        .child(
            h_flex()
                .p_3()
                .gap_2()
                .border_t_1()
                .border_color(theme.border)
                .child(div().flex_1().child(Input::new(&view.commit_input)))
                .child(
                    Button::new("commit")
                        .label("Commit")
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| this.commit_all(cx))),
                ),
        )
}

fn status_color(code: &str, theme: &gpui_component::theme::Theme) -> Hsla {
    match code {
        "??" => theme.muted_foreground,
        c if c.contains('D') => theme.danger,
        c if c.contains('A') => theme.success,
        _ => theme.warning,
    }
}
