//! Chat pane: transcript (streaming text, thinking, tool cards, notices) +
//! prompt input. Renders from the GPUI-free `Transcript` model.

use crate::transcript::Block;
use crate::views::root::RootView;
use gpui::*;
use gpui_component::button::Button;
use gpui_component::input::Input;
use gpui_component::scroll::ScrollableElement;
use gpui_component::{h_flex, v_flex, ActiveTheme, IconName};

pub fn render(
    view: &RootView,
    _window: &mut Window,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    let theme = cx.theme();
    let active = view.workspace.active_session.and_then(|i| view.workspace.sessions.get(i));

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
                    .child(div().text_sm().font_bold().child(s.handle.title.clone()))
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
                            render_block(i, b, cx)
                        })),
                )
                .vertical_scrollbar(&view.transcript_scroll)
                .into_any_element()
        }
        _ => v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .gap_2()
            .child(div().text_xl().font_bold().child("blurb"))
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("An agent, your repo, its own worktree. Describe a task to begin."),
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

    v_flex().size_full().min_w_0().child(header).child(transcript).child(input_row)
}

fn usage_line(s: &crate::workspace::SessionState) -> String {
    let u = &s.transcript.usage;
    if u.input_tokens == 0 && u.output_tokens == 0 {
        String::new()
    } else {
        format!(
            "{} turns · in {} (cache {}) · out {}",
            s.transcript.turns, u.input_tokens, u.cache_read_tokens, u.output_tokens
        )
    }
}

fn render_block(i: usize, block: Block, cx: &mut Context<RootView>) -> AnyElement {
    let theme = cx.theme();
    match block {
        Block::UserMessage { text } => div()
            .px_3()
            .py_2()
            .rounded(theme.radius)
            .bg(theme.secondary)
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
                .bg(theme.card)
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(div().text_xs().font_bold().child(format!("⏺ {name}")))
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
