//! Settings overlay: the provider registry, fully editable in-app.
//!
//! Two modes: the *list* (providers with their models; click a model to make
//! it the default for new sessions; add / duplicate / delete providers) and
//! the *editor* (every field of one provider: kind, endpoint, auth, extra
//! headers, extra body JSON, and its list of models). The settings TOML file
//! is persistence only — nothing requires hand-editing it.

use crate::settings::form;
use crate::views::root::RootView;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputState, Textarea, TextareaState};
use gpui_component::switch::Switch;
use gpui_component::{h_flex, v_flex, ActiveTheme, IconName, Sizable as _};
use harness_core::config::{ModelConfig, ProviderConfig, ProviderKind};
use harness_core::Effort;

// ---------------------------------------------------------------------------
// Editor state

pub struct ModelRow {
    pub effort: Option<Effort>,
    pub id: Entity<InputState>,
    pub label: Entity<InputState>,
    pub max_tokens: Entity<InputState>,
    pub temperature: Entity<InputState>,
    pub context_window: Entity<InputState>,
    pub extra_body: Entity<InputState>,
}

pub struct ProviderEditor {
    /// Index into `settings.providers` being edited.
    pub index: usize,
    pub kind: ProviderKind,
    pub supports_reasoning_effort: bool,
    pub name: Entity<InputState>,
    pub base_url: Entity<InputState>,
    pub api_key: Entity<InputState>,
    pub api_key_env: Entity<InputState>,
    pub extra_headers: Entity<TextareaState>,
    pub extra_body: Entity<TextareaState>,
    pub models: Vec<ModelRow>,
    pub error: Option<String>,
}

fn text_input(
    value: &str,
    placeholder: &'static str,
    window: &mut Window,
    cx: &mut Context<RootView>,
) -> Entity<InputState> {
    let value = value.to_string();
    cx.new(|cx| {
        let mut state = InputState::new(window, cx).placeholder(placeholder);
        if !value.is_empty() {
            state.set_value(value, window, cx);
        }
        state
    })
}

fn textarea(
    value: &str,
    placeholder: &'static str,
    window: &mut Window,
    cx: &mut Context<RootView>,
) -> Entity<TextareaState> {
    let value = value.to_string();
    cx.new(|cx| {
        let mut state = TextareaState::new(window, cx).placeholder(placeholder).auto_grow(2, 6);
        if !value.is_empty() {
            state.set_value(value, window, cx);
        }
        state
    })
}

fn model_row(
    m: &ModelConfig,
    window: &mut Window,
    cx: &mut Context<RootView>,
) -> ModelRow {
    ModelRow {
        effort: m.effort,
        id: text_input(&m.id, "model id (e.g. claude-opus-5)", window, cx),
        label: text_input(&m.label, "label (optional)", window, cx),
        max_tokens: text_input(&m.max_tokens.to_string(), "max tokens", window, cx),
        temperature: text_input(
            &m.temperature.map(|t| t.to_string()).unwrap_or_default(),
            "temp",
            window,
            cx,
        ),
        context_window: text_input(
            &m.context_window.map(|w| w.to_string()).unwrap_or_default(),
            "ctx window",
            window,
            cx,
        ),
        extra_body: text_input(
            &form::format_json_object(&m.extra_body).replace('\n', " "),
            "extra body JSON (optional)",
            window,
            cx,
        ),
    }
}

impl ProviderEditor {
    pub fn open(
        index: usize,
        p: &ProviderConfig,
        window: &mut Window,
        cx: &mut Context<RootView>,
    ) -> Self {
        Self {
            index,
            kind: p.kind,
            supports_reasoning_effort: p.supports_reasoning_effort,
            name: text_input(&p.name, "Provider name", window, cx),
            base_url: text_input(&p.base_url, "Base URL (empty = provider default)", window, cx),
            api_key: text_input(&p.api_key, "API key (prefer env var)", window, cx),
            api_key_env: text_input(&p.api_key_env, "API key env var", window, cx),
            extra_headers: textarea(
                &form::format_headers(&p.extra_headers),
                "One per line: Header-Name: value",
                window,
                cx,
            ),
            extra_body: textarea(
                &form::format_json_object(&p.extra_body),
                "JSON object merged into every request body",
                window,
                cx,
            ),
            models: p.models.iter().map(|m| model_row(m, window, cx)).collect(),
            error: None,
        }
    }

    /// Parse the form back into a ProviderConfig. Err = human-readable
    /// message, nothing is saved.
    fn to_config(&self, base: &ProviderConfig, cx: &App) -> Result<ProviderConfig, String> {
        let name = self.name.read(cx).value().trim().to_string();
        if name.is_empty() {
            return Err("provider name is required".into());
        }
        let mut models = Vec::new();
        for (i, row) in self.models.iter().enumerate() {
            let id = row.id.read(cx).value().trim().to_string();
            if id.is_empty() {
                continue; // blank row — ignore
            }
            let max_tokens_text = row.max_tokens.read(cx).value().trim().to_string();
            let max_tokens = if max_tokens_text.is_empty() {
                16_000
            } else {
                max_tokens_text
                    .parse::<u32>()
                    .map_err(|_| format!("model {}: max tokens must be a number", i + 1))?
            };
            let temp_text = row.temperature.read(cx).value().trim().to_string();
            let temperature = if temp_text.is_empty() {
                None
            } else {
                Some(
                    temp_text
                        .parse::<f64>()
                        .map_err(|_| format!("model {}: temperature must be a number", i + 1))?,
                )
            };
            let ctx_text = row.context_window.read(cx).value().trim().to_string();
            let context_window = if ctx_text.is_empty() {
                None
            } else {
                Some(ctx_text.parse::<u32>().map_err(|_| {
                    format!("model {}: context window must be a number of tokens", i + 1)
                })?)
            };
            let extra_body = form::parse_json_object(&row.extra_body.read(cx).value())
                .map_err(|e| format!("model {}: {e}", i + 1))?;
            models.push(ModelConfig {
                id,
                label: row.label.read(cx).value().trim().to_string(),
                effort: row.effort,
                temperature,
                max_tokens,
                context_window,
                extra_body,
            });
        }
        if models.is_empty() {
            return Err("at least one model (with an id) is required".into());
        }
        let mut config = base.clone();
        config.name = name;
        config.kind = self.kind;
        config.base_url = self.base_url.read(cx).value().trim().to_string();
        config.api_key = self.api_key.read(cx).value().trim().to_string();
        config.api_key_env = self.api_key_env.read(cx).value().trim().to_string();
        config.supports_reasoning_effort = self.supports_reasoning_effort;
        config.extra_headers = form::parse_headers(&self.extra_headers.read(cx).value())
            .map_err(|e| format!("headers: {e}"))?;
        config.extra_body = form::parse_json_object(&self.extra_body.read(cx).value())
            .map_err(|e| format!("extra body: {e}"))?;
        config.active_model = config.active_model.min(models.len() - 1);
        config.models = models;
        Ok(config)
    }
}

// ---------------------------------------------------------------------------
// RootView actions for the editor (kept here so root.rs stays lean)

impl RootView {
    pub fn open_provider_editor(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(p) = self.workspace.settings.providers.get(index).cloned() {
            self.provider_editor = Some(ProviderEditor::open(index, &p, window, cx));
            cx.notify();
        }
    }

    pub fn add_provider(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let index = self.workspace.settings.add_provider(ProviderKind::OpenAiCompat);
        self.open_provider_editor(index, window, cx);
    }

    pub fn duplicate_provider(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.workspace.settings.duplicate_provider(index).is_some() {
            let _ = self.workspace.settings.save();
            cx.notify();
        }
    }

    pub fn delete_provider(&mut self, index: usize, cx: &mut Context<Self>) {
        self.workspace.settings.remove_provider(index);
        let _ = self.workspace.settings.save();
        self.provider_editor = None;
        cx.notify();
    }

    pub fn close_provider_editor(&mut self, cx: &mut Context<Self>) {
        // A just-added provider that was never saved (still blank) is
        // removed again on cancel.
        if let Some(ed) = self.provider_editor.take() {
            if let Some(p) = self.workspace.settings.providers.get(ed.index) {
                if p.models.len() == 1 && p.models[0].id.is_empty() {
                    self.workspace.settings.remove_provider(ed.index);
                }
            }
        }
        cx.notify();
    }

    pub fn save_provider_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &self.provider_editor else { return };
        let Some(base) = self.workspace.settings.providers.get(editor.index).cloned() else {
            self.provider_editor = None;
            cx.notify();
            return;
        };
        match editor.to_config(&base, cx) {
            Ok(mut config) => {
                config.normalize();
                self.workspace.settings.providers[editor.index] = config;
                let _ = self.workspace.settings.save();
                self.provider_editor = None;
            }
            Err(e) => {
                if let Some(editor) = &mut self.provider_editor {
                    editor.error = Some(e);
                }
            }
        }
        cx.notify();
    }

    pub fn editor_add_model(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.provider_editor {
            let blank = ModelConfig::new("");
            let row = model_row(&blank, window, cx);
            editor.models.push(row);
            cx.notify();
        }
    }

    pub fn editor_remove_model(&mut self, row: usize, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.provider_editor {
            if editor.models.len() > 1 && row < editor.models.len() {
                editor.models.remove(row);
                cx.notify();
            }
        }
    }

    pub fn editor_cycle_effort(&mut self, row: usize, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.provider_editor {
            if let Some(m) = editor.models.get_mut(row) {
                m.effort = next_effort(m.effort);
                cx.notify();
            }
        }
    }

    pub fn editor_set_kind(&mut self, kind: ProviderKind, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.provider_editor {
            editor.kind = kind;
            cx.notify();
        }
    }

    pub fn editor_toggle_reasoning_effort(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.provider_editor {
            editor.supports_reasoning_effort = !editor.supports_reasoning_effort;
            cx.notify();
        }
    }
}

fn next_effort(e: Option<Effort>) -> Option<Effort> {
    match e {
        None => Some(Effort::Low),
        Some(Effort::Low) => Some(Effort::Medium),
        Some(Effort::Medium) => Some(Effort::High),
        Some(Effort::High) => Some(Effort::XHigh),
        Some(Effort::XHigh) => Some(Effort::Max),
        Some(Effort::Max) => None,
    }
}

fn effort_label(e: Option<Effort>) -> &'static str {
    match e {
        None => "effort: —",
        Some(Effort::Low) => "effort: low",
        Some(Effort::Medium) => "effort: med",
        Some(Effort::High) => "effort: high",
        Some(Effort::XHigh) => "effort: xhigh",
        Some(Effort::Max) => "effort: max",
    }
}

// ---------------------------------------------------------------------------
// Rendering

pub fn render(view: &RootView, cx: &mut Context<RootView>) -> impl IntoElement {
    let theme = cx.theme();
    let editing = view.provider_editor.is_some();

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
                .w(px(640.))
                .max_h(px(720.))
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
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
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::BOLD)
                                .child(if editing { "Edit provider" } else { "Providers" }),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .when(!editing, |this| {
                                    this.child(
                                        Button::new("add-provider")
                                            .icon(IconName::Plus)
                                            .label("Add provider")
                                            .ghost()
                                            .small()
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.add_provider(window, cx)
                                            })),
                                    )
                                })
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
                .child(if editing {
                    render_editor(view, cx).into_any_element()
                } else {
                    render_list(view, cx).into_any_element()
                }),
        )
}

fn render_list(view: &RootView, cx: &mut Context<RootView>) -> impl IntoElement {
    let theme = cx.theme();
    let active_provider = view.workspace.settings.active_provider;
    let providers = view.workspace.settings.providers.clone();
    let worktrees_on = view.workspace.settings.sessions_use_worktrees;

    v_flex()
        .child(
            v_flex().id("provider-list").p_3().gap_3().max_h(px(560.)).overflow_y_scroll().children(
                providers.into_iter().enumerate().map(|(i, p)| {
                    let is_active_provider = i == active_provider;
                    v_flex()
                        .px_3()
                        .py_2()
                        .gap_2()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(if is_active_provider { theme.primary } else { theme.border })
                        // Header row: name, kind, actions
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .justify_between()
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(FontWeight::BOLD)
                                                .child(p.name.clone()),
                                        )
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
                                    h_flex()
                                        .gap_1()
                                        .child(
                                            Button::new(("edit-provider", i))
                                                .label("Edit")
                                                .ghost()
                                                .xsmall()
                                                .on_click(cx.listener(move |this, _, window, cx| {
                                                    this.open_provider_editor(i, window, cx)
                                                })),
                                        )
                                        .child(
                                            Button::new(("dup-provider", i))
                                                .icon(IconName::Copy)
                                                .ghost()
                                                .xsmall()
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.duplicate_provider(i, cx)
                                                })),
                                        )
                                        .child(
                                            Button::new(("del-provider", i))
                                                .icon(IconName::Delete)
                                                .ghost()
                                                .xsmall()
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.delete_provider(i, cx)
                                                })),
                                        ),
                                ),
                        )
                        // Connection summary
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(connection_summary(&p)),
                        )
                        // Model rows — click to select as the default
                        .children(p.models.iter().enumerate().map(|(j, m)| {
                            let selected = is_active_provider && j == p.active_model;
                            let effort = m.effort;
                            h_flex()
                                .id(("model", i * 100 + j))
                                .px_2()
                                .py_1()
                                .gap_2()
                                .items_center()
                                .rounded(theme.radius)
                                .cursor_pointer()
                                .when(selected, |d| d.bg(theme.accent))
                                .hover(|d| d.bg(theme.accent))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.select_provider_model(i, Some(j), cx)
                                }))
                                .child(
                                    div()
                                        .size_1p5()
                                        .rounded_full()
                                        .bg(if selected { theme.primary } else { theme.muted }),
                                )
                                .child(div().text_sm().flex_1().truncate().child(
                                    m.display_label().to_string(),
                                ))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(format!(
                                            "{} · {}k tok",
                                            effort_label(effort),
                                            m.max_tokens / 1000
                                        )),
                                )
                        }))
                }),
            ),
        )
        // Permissions
        .child({
            let policy = view.workspace.settings.permissions.clone();
            v_flex()
                .px_4()
                .py_3()
                .gap_2()
                .border_t_1()
                .border_color(theme.border)
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::BOLD)
                        .text_color(theme.muted_foreground)
                        .child("PERMISSIONS"),
                )
                .child(
                    h_flex()
                        .gap_4()
                        .items_center()
                        .child(
                            Switch::new("perm-ask")
                                .checked(policy.ask)
                                .label("Ask before edits & commands")
                                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                    this.workspace.settings.permissions.ask = *checked;
                                    this.save_settings(cx);
                                })),
                        )
                        .child(
                            Switch::new("perm-edits")
                                .checked(policy.allow_edits)
                                .label("Pre-approve file edits")
                                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                    this.workspace.settings.permissions.allow_edits = *checked;
                                    this.save_settings(cx);
                                })),
                        ),
                )
                .child(if policy.allowed_bash.is_empty() {
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(
                            "No pre-approved commands. Choosing \"Always allow\" on a prompt \
                             adds its program here.",
                        )
                        .into_any_element()
                } else {
                    h_flex()
                        .gap_1()
                        .flex_wrap()
                        .children(policy.allowed_bash.into_iter().enumerate().map(|(k, cmd)| {
                            h_flex()
                                .gap_1()
                                .items_center()
                                .px_1p5()
                                .rounded(theme.radius)
                                .bg(theme.muted)
                                .text_xs()
                                .child(cmd)
                                .child(
                                    Button::new(("rm-bash", k))
                                        .icon(IconName::Close)
                                        .ghost()
                                        .xsmall()
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.remove_allowed_bash(k, cx)
                                        })),
                                )
                        }))
                        .into_any_element()
                })
        })
        .child(
            h_flex()
                .px_4()
                .py_3()
                .items_center()
                .justify_between()
                .border_t_1()
                .border_color(theme.border)
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("Click a model to use it for new sessions."),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("Max turns/run"),
                        )
                        .child(div().w(px(70.)).child(Input::new(&view.max_turns_input))),
                )
                .child(
                    Switch::new("worktree-isolation")
                        .checked(worktrees_on)
                        .label("Worktree-isolated sessions")
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.workspace.settings.sessions_use_worktrees = *checked;
                            this.save_settings(cx);
                        })),
                ),
        )
}

fn connection_summary(p: &ProviderConfig) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !p.base_url.is_empty() {
        parts.push(p.base_url.clone());
    }
    if !p.api_key_env.is_empty() {
        parts.push(format!("key: ${}", p.api_key_env));
    } else if !p.api_key.is_empty() {
        parts.push("key: (stored)".into());
    }
    if !p.extra_headers.is_empty() {
        parts.push(format!("{} header(s)", p.extra_headers.len()));
    }
    if !p.extra_body.is_empty() {
        parts.push(format!("{} body field(s)", p.extra_body.len()));
    }
    if parts.is_empty() {
        "defaults".into()
    } else {
        parts.join(" · ")
    }
}

fn field(label: &'static str, input: impl IntoElement, theme: &gpui_component::theme::Theme) -> Div {
    v_flex()
        .gap_1()
        .child(div().text_xs().text_color(theme.muted_foreground).child(label))
        .child(input.into_any_element())
}

fn render_editor(view: &RootView, cx: &mut Context<RootView>) -> impl IntoElement {
    let theme = cx.theme();
    let Some(editor) = &view.provider_editor else {
        return v_flex().into_any_element();
    };
    let kind = editor.kind;
    let index = editor.index;
    let error = editor.error.clone();
    let model_count = editor.models.len();

    v_flex()
        .child(
            v_flex()
                .id("provider-editor")
                .p_4()
                .gap_3()
                .max_h(px(560.))
                .overflow_y_scroll()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::BOLD)
                        .text_color(theme.muted_foreground)
                        .child("CONNECTION"),
                )
                // Kind selector
                .child(
                    h_flex().gap_1().children(ProviderKind::ALL.into_iter().enumerate().map(
                        |(k, variant)| {
                            let selected = kind == variant;
                            Button::new(("kind", k))
                                .label(variant.label())
                                .small()
                                .map(|b| if selected { b.primary() } else { b.ghost() })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.editor_set_kind(variant, cx)
                                }))
                        },
                    )),
                )
                .child(field("Name", Input::new(&editor.name), theme))
                .child(field("Base URL", Input::new(&editor.base_url), theme))
                .child(
                    h_flex()
                        .gap_3()
                        .child(div().flex_1().child(field(
                            "API key env var (preferred)",
                            Input::new(&editor.api_key_env),
                            theme,
                        )))
                        .child(div().flex_1().child(field(
                            "API key (stored in settings file)",
                            Input::new(&editor.api_key),
                            theme,
                        ))),
                )
                .child(
                    Switch::new("sre")
                        .checked(editor.supports_reasoning_effort)
                        .label("Server supports reasoning_effort (OpenAI-compatible)")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.editor_toggle_reasoning_effort(cx)
                        })),
                )
                .child(field("Extra HTTP headers", Textarea::new(&editor.extra_headers), theme))
                .child(field(
                    "Extra request-body fields (JSON object)",
                    Textarea::new(&editor.extra_body),
                    theme,
                ))
                // Models
                .child(
                    h_flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::BOLD)
                                .text_color(theme.muted_foreground)
                                .child("MODELS"),
                        )
                        .child(
                            Button::new("add-model")
                                .icon(IconName::Plus)
                                .label("Add model")
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.editor_add_model(window, cx)
                                })),
                        ),
                )
                .children(editor.models.iter().enumerate().map(|(j, row)| {
                    v_flex()
                        .gap_1()
                        .px_2()
                        .py_2()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.border)
                        .child(
                            h_flex()
                                .gap_2()
                                .child(div().flex_1().child(Input::new(&row.id)))
                                .child(div().w(px(140.)).child(Input::new(&row.label)))
                                .child(
                                    Button::new(("effort", j))
                                        .label(effort_label(row.effort))
                                        .ghost()
                                        .xsmall()
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.editor_cycle_effort(j, cx)
                                        })),
                                )
                                .when(model_count > 1, |this| {
                                    this.child(
                                        Button::new(("rm-model", j))
                                            .icon(IconName::Delete)
                                            .ghost()
                                            .xsmall()
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.editor_remove_model(j, cx)
                                            })),
                                    )
                                }),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(div().w(px(100.)).child(Input::new(&row.max_tokens)))
                                .child(div().w(px(70.)).child(Input::new(&row.temperature)))
                                .child(div().w(px(100.)).child(Input::new(&row.context_window)))
                                .child(div().flex_1().child(Input::new(&row.extra_body))),
                        )
                })),
        )
        // Footer: error + actions
        .child(
            h_flex()
                .px_4()
                .py_3()
                .gap_2()
                .items_center()
                .justify_between()
                .border_t_1()
                .border_color(theme.border)
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("delete-provider-editor")
                                .icon(IconName::Delete)
                                .label("Delete provider")
                                .ghost()
                                .small()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.delete_provider(index, cx)
                                })),
                        )
                        .when_some(error, |this, e| {
                            this.child(div().text_xs().text_color(theme.danger).child(e))
                        }),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(Button::new("cancel-editor").label("Cancel").ghost().small().on_click(
                            cx.listener(|this, _, _, cx| this.close_provider_editor(cx)),
                        ))
                        .child(
                            Button::new("save-editor").label("Save").primary().small().on_click(
                                cx.listener(|this, _, _, cx| this.save_provider_editor(cx)),
                            ),
                        ),
                ),
        )
        .into_any_element()
}
