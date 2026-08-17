//! Settings overlay: provider profile picker + summary of each profile's
//! customizations (extra headers / extra body fields). Profiles themselves
//! live in settings.toml — a full in-app editor is on the roadmap; the
//! "Open settings file" button jumps there.

use crate::settings::Settings;
use crate::views::root::RootView;
use gpui::*;
use gpui_component::button::Button;
use gpui_component::{h_flex, v_flex, ActiveTheme, IconName};

pub fn render(view: &RootView, cx: &mut Context<RootView>) -> impl IntoElement {
    let theme = cx.theme();
    let active = view.workspace.settings.active_provider;
    let providers = view.workspace.settings.providers.clone();

    // Dimmed backdrop + centered card.
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui::black().opacity(0.5))
        .id("settings-backdrop")
        .on_click(cx.listener(|this, _, _, cx| this.toggle_settings(cx)))
        .child(
            v_flex()
                .id("settings-card")
                .w(px(560.))
                .max_h(px(640.))
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .bg(theme.card)
                .shadow_lg()
                .overflow_hidden()
                // Swallow clicks so the backdrop doesn't close on card clicks.
                .on_click(cx.listener(|_, _, _, _| {}))
                .child(
                    h_flex()
                        .px_4()
                        .py_3()
                        .items_center()
                        .justify_between()
                        .border_b_1()
                        .border_color(theme.border)
                        .child(div().text_sm().font_bold().child("Providers"))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("open-settings-file")
                                        .label("Open settings file")
                                        .ghost()
                                        .small()
                                        .on_click(cx.listener(|_, _, _, cx| {
                                            let path = Settings::path();
                                            cx.open_url(&format!("file://{}", path.display()));
                                        })),
                                )
                                .child(
                                    Button::new("close-settings")
                                        .icon(IconName::Close)
                                        .ghost()
                                        .small()
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.toggle_settings(cx)),
                                        ),
                                ),
                        ),
                )
                .child(v_flex().p_3().gap_2().children(
                    providers.into_iter().enumerate().map(|(i, p)| {
                        let selected = i == active;
                        div()
                            .id(("provider", i))
                            .px_3()
                            .py_2()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(if selected { theme.primary } else { theme.border })
                            .cursor_pointer()
                            .hover(|d| d.bg(theme.accent))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.set_active_provider(i, cx)),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .items_center()
                                            .child(div().text_sm().font_bold().child(p.name.clone()))
                                            .child(
                                                div()
                                                    .px_1p5()
                                                    .rounded(theme.radius)
                                                    .bg(theme.muted)
                                                    .text_xs()
                                                    .child(p.kind.label()),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(p.model.clone()),
                                    ),
                            )
                            .child(
                                div().text_xs().text_color(theme.muted_foreground).child(
                                    profile_summary(&p),
                                ),
                            )
                    }),
                ))
                .child(
                    div()
                        .px_4()
                        .py_3()
                        .border_t_1()
                        .border_color(theme.border)
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(
                            "Profiles (base URL, keys, models, extra headers, extra body fields) \
                             are edited in settings.toml; changes apply to new sessions.",
                        ),
                ),
        )
}

fn profile_summary(p: &harness_core::config::ProviderConfig) -> String {
    let mut parts: Vec<String> = Vec::new();
    let base = p.resolved_base_url();
    if !base.is_empty() {
        parts.push(base);
    }
    if let Some(e) = p.effort {
        parts.push(format!("effort={e:?}").to_lowercase());
    }
    if !p.extra_headers.is_empty() {
        parts.push(format!("{} extra header(s)", p.extra_headers.len()));
    }
    if !p.extra_body.is_empty() {
        parts.push(format!("{} extra body field(s)", p.extra_body.len()));
    }
    if parts.is_empty() {
        "defaults".into()
    } else {
        parts.join(" · ")
    }
}
