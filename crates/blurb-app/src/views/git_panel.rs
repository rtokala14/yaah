//! Git panel: branch/HEAD (with ahead/behind), working-tree status,
//! diffstat, lane-colored commit history graph, worktrees (with owning
//! sessions, merge + remove actions), commit box. Renders from the immutable
//! RepoSnapshot the background refresher produces.

use crate::views::root::RootView;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::Input;
use gpui_component::{h_flex, v_flex, ActiveTheme, Sizable as _, IconName};
use harness_git::GraphRow;

/// Colors for graph lanes, cycled. Chosen to read on both dark and light
/// panels without clashing with the status colors.
fn lane_color(lane: usize) -> Hsla {
    const LANES: [(f32, f32, f32); 6] = [
        (210. / 360., 0.70, 0.60), // blue
        (140. / 360., 0.55, 0.50), // green
        (35. / 360., 0.80, 0.60),  // amber
        (280. / 360., 0.55, 0.65), // violet
        (0. / 360., 0.65, 0.62),   // red
        (180. / 360., 0.50, 0.50), // teal
    ];
    let (h, s, l) = LANES[lane % LANES.len()];
    hsla(h, s, l, 1.0)
}

const MAX_GRAPH_LANES: usize = 6;
const GRAPH_ROWS: usize = 30;

pub fn render(view: &RootView, cx: &mut Context<RootView>) -> impl IntoElement {
    let theme = cx.theme();
    let wt_sessions = view.workspace.worktree_sessions();

    let body: AnyElement = match (&view.workspace.git, &view.workspace.git_error) {
        (Some(snap), _) => {
            let branch = snap.head_branch.clone().unwrap_or_else(|| "(detached)".into());
            let head_info =
                snap.branches.iter().find(|b| b.is_head).cloned();
            let statuses = snap.statuses.clone();
            let worktrees = snap.worktrees.clone();
            let diff = snap.diff.clone();
            let log: Vec<GraphRow> = snap.log.iter().take(GRAPH_ROWS).cloned().collect();
            let head_branch = snap.head_branch.clone();

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
                                .child(
                                    div().text_sm().text_color(theme.muted_foreground).child("⎇"),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::BOLD)
                                        .child(branch),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(snap.head_short_id.clone()),
                                )
                                .when_some(head_info, |this, b| {
                                    this.when_some(b.ahead.zip(b.behind), |this, (a, be)| {
                                        this.child(
                                            div()
                                                .px_1p5()
                                                .rounded(theme.radius)
                                                .bg(theme.muted)
                                                .text_xs()
                                                .child(format!("↑{a} ↓{be}")),
                                        )
                                    })
                                }),
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
                // Status list — click a row to review its diff
                .child(
                    v_flex().gap_0p5().overflow_hidden().children(
                        statuses.into_iter().take(30).enumerate().map(|(i, s)| {
                            let path = s.path.clone();
                            h_flex()
                                .id(("status", i))
                                .gap_2()
                                .text_xs()
                                .cursor_pointer()
                                .rounded(theme.radius)
                                .hover(|d| d.bg(theme.accent))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_file_diff(path.clone(), cx)
                                }))
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
                // History graph
                .child(
                    v_flex()
                        .gap_0p5()
                        .flex_1()
                        .overflow_hidden()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::BOLD)
                                .text_color(theme.muted_foreground)
                                .child("HISTORY"),
                        )
                        .child(
                            v_flex()
                                .id("git-history")
                                .gap_0()
                                .overflow_y_scroll()
                                .children(log.into_iter().map(|row| graph_row(row, theme))),
                        ),
                )
                // Worktrees
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::BOLD)
                                .text_color(theme.muted_foreground)
                                .child("WORKTREES"),
                        )
                        .children(worktrees.into_iter().map(|wt| {
                            let session = wt_sessions.get(&wt.name).copied();
                            let name = wt.name.clone();
                            let wt_branch = wt.branch.clone();
                            let mergeable = wt_branch
                                .as_deref()
                                .map(|b| head_branch.as_deref() != Some(b))
                                .unwrap_or(false);
                            h_flex()
                                .gap_1()
                                .items_center()
                                .text_xs()
                                .child(div().flex_1().truncate().child(format!(
                                    "{} ⎇ {}",
                                    wt.name,
                                    wt_branch.clone().unwrap_or_default()
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
                                .when(mergeable, |this| {
                                    let branch = wt_branch.clone().unwrap_or_default();
                                    let wt_name = name.clone();
                                    this.child(
                                        Button::new(SharedString::from(format!("merge-{name}")))
                                            .label("Merge")
                                            .ghost()
                                            .xsmall()
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.merge_branch(
                                                    branch.clone(),
                                                    Some(wt_name.clone()),
                                                    cx,
                                                );
                                            })),
                                    )
                                })
                                .child(
                                    Button::new(SharedString::from(format!("rm-wt-{name}")))
                                        .icon(IconName::Delete)
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
                .child(div().text_sm().font_weight(FontWeight::BOLD).child("Git")),
        )
        .child(body)
        // Post-merge cleanup offer
        .when_some(
            view.merge_cleanup.as_ref().map(|c| c.branch.clone()),
            |this, branch| {
                this.child(
                    h_flex()
                        .px_3()
                        .py_1p5()
                        .gap_2()
                        .items_center()
                        .border_t_1()
                        .border_color(theme.border)
                        .child(
                            div()
                                .text_xs()
                                .flex_1()
                                .truncate()
                                .child(format!("{branch} is merged — clean up?")),
                        )
                        .child(
                            Button::new("do-cleanup")
                                .label("Remove worktree + branch")
                                .primary()
                                .xsmall()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.perform_merge_cleanup(cx)
                                })),
                        )
                        .child(
                            Button::new("dismiss-cleanup")
                                .icon(IconName::Close)
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.dismiss_merge_cleanup(cx)
                                })),
                        ),
                )
            },
        )
        // Last operation outcome
        .when_some(view.git_op_status.clone(), |this, msg| {
            this.child(
                div()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .border_t_1()
                    .border_color(theme.border)
                    .truncate()
                    .child(msg),
            )
        })
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
                        .primary()
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| this.commit_all(cx))),
                ),
        )
}

/// One history row: lane cells (dot / vertical line) + id + refs + summary.
fn graph_row(row: GraphRow, theme: &gpui_component::theme::Theme) -> impl IntoElement {
    let lanes = row.active.len().max(row.lane + 1).min(MAX_GRAPH_LANES);
    let refs = row.refs.clone();
    let is_merge = row.parent_count > 1;
    h_flex()
        .gap_2()
        .items_center()
        .h(px(20.))
        // Graph cells
        .child(
            h_flex().gap_0().children((0..lanes).map(|l| {
                let cell = div().w(px(10.)).h(px(20.)).flex().items_center().justify_center();
                if l == row.lane {
                    cell.child(
                        div()
                            .size(px(7.))
                            .rounded_full()
                            .when(is_merge, |d| d.rounded(px(1.)))
                            .bg(lane_color(l)),
                    )
                } else if row.active.get(l).copied().unwrap_or(false) {
                    cell.child(div().w(px(2.)).h_full().bg(lane_color(l).opacity(0.55)))
                } else {
                    cell
                }
            })),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(row.short_id.clone()),
        )
        // Branch tips
        .children(refs.into_iter().take(2).map(|r| {
            div()
                .px_1()
                .rounded(theme.radius)
                .bg(theme.muted)
                .text_xs()
                .max_w(px(90.))
                .truncate()
                .child(r)
        }))
        .child(div().text_xs().flex_1().truncate().child(row.summary.clone()))
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(rel_time(row.time_unix)),
        )
}

/// "now" / "5m" / "3h" / "2d" style relative timestamps.
fn rel_time(unix: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let dt = (now - unix).max(0);
    match dt {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m", dt / 60),
        3600..=86_399 => format!("{}h", dt / 3600),
        _ => format!("{}d", dt / 86_400),
    }
}

fn status_color(code: &str, theme: &gpui_component::theme::Theme) -> Hsla {
    match code {
        "??" => theme.muted_foreground,
        c if c.contains('D') => theme.danger,
        c if c.contains('A') => theme.success,
        _ => theme.warning,
    }
}
