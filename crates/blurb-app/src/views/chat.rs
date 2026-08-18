//! Chat pane: transcript (streaming text, thinking, tool cards, notices) +
//! prompt input. Renders from the GPUI-free `Transcript` model.

use crate::transcript::Block;
use crate::views::root::RootView;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::Input;
use gpui_component::scroll::ScrollableElement;
use gpui_component::{h_flex, v_flex, ActiveTheme, IconName, Sizable as _};

pub fn render(
    view: &RootView,
    _window: &mut Window,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    // Cloned: interactive blocks need `cx` for listeners while styling reads
    // the theme.
    let theme = cx.theme().clone();
    let theme = &theme;
    let active = view.workspace.active_session.and_then(|i| view.workspace.sessions.get(i));
    let todos = active.map(|s| s.transcript.todos.clone()).unwrap_or_default();
    // Offer switching the running session to the settings-selected default
    // when they differ.
    let default_profile = view.workspace.settings.active_profile();
    let switch_offer = active
        .filter(|s| s.provider_label != default_profile.label())
        .map(|_| default_profile.label());

    let header: AnyElement = match active {
        Some(s) => h_flex()
            .px_4()
            .py_2()
            .gap_2()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().text_sm().font_weight(FontWeight::BOLD).child(s.handle.title.clone()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(s.provider_label.clone()),
                    )
                    .when_some(s.worktree.clone(), |this, wt| {
                        this.child(
                            div()
                                .px_1p5()
                                .rounded(theme.radius)
                                .bg(theme.muted)
                                .text_xs()
                                .child(format!("⎇ {wt}")),
                        )
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div().text_xs().text_color(theme.muted_foreground).child(usage_line(s)),
                    )
                    .child({
                        let planning = s.transcript.plan_mode;
                        Button::new("plan-mode")
                            .label(if planning { "Planning" } else { "Plan" })
                            .map(|b| if planning { b.primary() } else { b.ghost() })
                            .xsmall()
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_plan_mode(cx)))
                    })
                    .when_some(switch_offer, |this, label| {
                        this.child(
                            Button::new("switch-profile")
                                .label(format!("→ {label}"))
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.switch_active_session_profile(cx)
                                })),
                        )
                    })
                    .when(s.transcript.running, |this| {
                        this.child(
                            Button::new("interrupt")
                                .icon(IconName::CircleX)
                                .danger()
                                .small()
                                .label("Stop")
                                .on_click(cx.listener(|this, _, _, cx| this.interrupt_active(cx))),
                        )
                    }),
            )
            .into_any_element(),
        None => h_flex()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("No session — type below to start one"),
            )
            .into_any_element(),
    };

    let transcript: AnyElement = match active {
        Some(s) if !s.transcript.blocks.is_empty() => {
            let blocks = s.transcript.blocks.clone();
            div()
                .id("transcript")
                .flex_1()
                .overflow_y_scroll()
                .track_scroll(&view.transcript_scroll)
                .child(
                    v_flex()
                        .p_4()
                        .gap_3()
                        .children(blocks.into_iter().enumerate().map(|(i, b)| {
                            render_block(i, b, theme, cx)
                        })),
                )
                .vertical_scrollbar(&view.transcript_scroll)
                .into_any_element()
        }
        _ => v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                div()
                    .text_2xl()
                    .font_weight(FontWeight::BOLD)
                    .child("blurb"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("An agent, your repo, its own worktree."),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .px_2()
                            .py_0p5()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(theme.border)
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(view.workspace.settings.active_profile().label()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("· describe a task below to begin"),
                    ),
            )
            .into_any_element(),
    };

    let input_row = h_flex()
        .p_3()
        .gap_2()
        .items_center()
        .border_t_1()
        .border_color(theme.border)
        .child(div().flex_1().child(Input::new(&view.prompt_input)))
        .child(
            Button::new("send")
                .icon(IconName::ArrowUp)
                .primary()
                .on_click(cx.listener(|this, _, window, cx| {
                    let text = this.prompt_input.read(cx).value().trim().to_string();
                    if !text.is_empty() {
                        this.send_prompt(text, window, cx);
                        this.prompt_input
                            .update(cx, |state, cx| state.set_value("", window, cx));
                    }
                })),
        );

    // Live plan panel (todo_write): compact, auto-hides when empty.
    let todo_panel: Option<AnyElement> = if todos.is_empty() {
        None
    } else {
        let done = todos
            .iter()
            .filter(|t| t.status == harness_core::types::TodoStatus::Done)
            .count();
        Some(
            v_flex()
                .px_4()
                .py_2()
                .gap_1()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::BOLD)
                        .text_color(theme.muted_foreground)
                        .child(format!("PLAN · {done}/{}", todos.len())),
                )
                .children(todos.into_iter().map(|t| {
                    use harness_core::types::TodoStatus;
                    let (glyph, color) = match t.status {
                        TodoStatus::Done => ("✓", theme.success),
                        TodoStatus::InProgress => ("▶", theme.warning),
                        TodoStatus::Pending => ("○", theme.muted_foreground),
                    };
                    h_flex()
                        .gap_2()
                        .items_center()
                        .text_xs()
                        .child(div().w(px(14.)).text_color(color).child(glyph))
                        .child(
                            div()
                                .flex_1()
                                .truncate()
                                .when(t.status == TodoStatus::Done, |d| {
                                    d.text_color(theme.muted_foreground)
                                })
                                .child(t.text),
                        )
                }))
                .into_any_element(),
        )
    };

    v_flex()
        .size_full()
        .min_w_0()
        .child(header)
        .children(todo_panel)
        .child(transcript)
        .child(input_row)
}

fn usage_line(s: &crate::workspace::SessionState) -> String {
    let u = &s.transcript.usage;
    if u.input_tokens == 0 && u.output_tokens == 0 && u.cache_read_tokens == 0 {
        return String::new();
    }
    let prompt_total = u.input_tokens + u.cache_read_tokens;
    let cache_pct = if prompt_total > 0 {
        (u.cache_read_tokens as f64 / prompt_total as f64 * 100.).round() as u64
    } else {
        0
    };
    let mut line = format!(
        "{} turns · in {} ({cache_pct}% cached) · out {}",
        s.transcript.turns, prompt_total, u.output_tokens
    );
    if s.transcript.context_budget > 0 {
        let pct = (s.transcript.context_tokens as f64 / s.transcript.context_budget as f64
            * 100.)
            .round() as u64;
        line.push_str(&format!(" · ctx {pct}%"));
    }
    if s.transcript.memory_count > 0 {
        line.push_str(&format!(" · ◆ {}", s.transcript.memory_count));
    }
    line
}

fn render_block(
    i: usize,
    block: Block,
    theme: &gpui_component::theme::Theme,
    cx: &mut Context<RootView>,
) -> AnyElement {
    match block {
        Block::Question { id, question, options, answer } => v_flex()
            .px_3()
            .py_2()
            .gap_2()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.primary)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().text_sm().text_color(theme.primary).child("?"))
                    .child(div().text_sm().font_weight(FontWeight::BOLD).child(question)),
            )
            .child(match answer {
                Some(a) => div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("answered: {a}"))
                    .into_any_element(),
                None => h_flex()
                    .gap_2()
                    .items_center()
                    .flex_wrap()
                    .children(options.into_iter().enumerate().map(|(k, opt)| {
                        let reply = opt.clone();
                        Button::new(("q-opt", i * 100 + k)).label(opt).small().on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.answer_question(id, reply.clone(), cx)
                            }),
                        )
                    }))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("…or type a reply below"),
                    )
                    .into_any_element(),
            })
            .into_any_element(),
        Block::Permission { id, tool, summary, decision } => v_flex()
            .px_3()
            .py_2()
            .gap_2()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.warning)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::BOLD)
                            .child(format!("{tool} wants to run:")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .font_family("monospace")
                            .truncate()
                            .child(summary),
                    ),
            )
            .child(match decision {
                Some(d) => div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(d)
                    .into_any_element(),
                None => h_flex()
                    .gap_2()
                    .child(
                        Button::new(("perm-allow", i))
                            .label("Allow")
                            .primary()
                            .small()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.permission_decision(
                                    id,
                                    harness_core::types::PermissionDecision::Allow,
                                    cx,
                                )
                            })),
                    )
                    .child(
                        Button::new(("perm-always", i))
                            .label("Always allow")
                            .ghost()
                            .small()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.permission_decision(
                                    id,
                                    harness_core::types::PermissionDecision::AllowAlways,
                                    cx,
                                )
                            })),
                    )
                    .child(
                        Button::new(("perm-deny", i))
                            .label("Deny")
                            .danger()
                            .small()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.permission_decision(
                                    id,
                                    harness_core::types::PermissionDecision::Deny,
                                    cx,
                                )
                            })),
                    )
                    .into_any_element(),
            })
            .into_any_element(),
        Block::UserMessage { text } => div()
            .px_3()
            .py_2()
            .rounded(theme.radius)
            .bg(theme.secondary)
            .border_l_2()
            .border_color(theme.primary)
            .text_sm()
            .child(text)
            .into_any_element(),
        Block::AssistantText { text, streaming } => div()
            .text_sm()
            .child(text)
            .when(streaming, |d| d.opacity(0.9))
            .into_any_element(),
        Block::Thinking { text, streaming } => div()
            .px_3()
            .py_2()
            .border_l_2()
            .border_color(theme.border)
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(text)
            .when(streaming, |d| d.opacity(0.8))
            .into_any_element(),
        Block::ToolCall { name, input_summary, output_preview, is_error, duration_ms, done } => {
            v_flex()
                .id(("tool", i))
                .px_3()
                .py_2()
                .gap_1()
                .rounded(theme.radius)
                .border_1()
                .border_color(if is_error { theme.danger } else { theme.border })
                .bg(theme.popover)
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(div().size(px(6.)).rounded_full().bg(if !done {
                            theme.warning
                        } else if is_error {
                            theme.danger
                        } else {
                            theme.success
                        }))
                        .child(div().text_xs().font_weight(FontWeight::BOLD).child(name.clone()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .flex_1()
                                .truncate()
                                .child(input_summary),
                        )
                        .child(div().text_xs().text_color(theme.muted_foreground).child(
                            if done { format!("{duration_ms}ms") } else { "…".to_string() },
                        )),
                )
                .when(!output_preview.is_empty(), |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(if is_error {
                                theme.danger
                            } else {
                                theme.muted_foreground
                            })
                            .truncate()
                            .child(output_preview),
                    )
                })
                .into_any_element()
        }
        Block::Notice { text } => div()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(format!("· {text}"))
            .into_any_element(),
        Block::Error { text } => div()
            .px_3()
            .py_2()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.danger)
            .text_xs()
            .text_color(theme.danger)
            .child(text)
            .into_any_element(),
    }
}
