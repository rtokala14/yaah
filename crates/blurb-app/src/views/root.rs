//! Root view: three-pane resizable layout (sidebar | chat | git panel),
//! owns the Workspace model and the two background loops (session event
//! pump, git refresh).

use crate::settings::Settings;
use crate::views::settings_view::{self, ProviderEditor};
use crate::views::{chat, git_panel, sidebar};
use crate::workspace::Workspace;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::input::{InputEvent, InputState};
use gpui_component::resizable::{h_resizable, resizable_panel, ResizableState};
use gpui_component::ActiveTheme;
use harness_git::MergeOutcome;
use std::path::PathBuf;
use std::time::Duration;

pub struct RootView {
    pub workspace: Workspace,
    pub prompt_input: Entity<InputState>,
    pub commit_input: Entity<InputState>,
    pub layout: Entity<ResizableState>,
    pub show_settings: bool,
    /// In-app provider editor state (Some while editing a provider).
    pub provider_editor: Option<ProviderEditor>,
    /// Outcome of the last git operation (commit/merge), shown in the panel.
    pub git_op_status: Option<String>,
    pub transcript_scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl RootView {
    pub fn new(
        project_root: PathBuf,
        settings: Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let workspace = Workspace::open(project_root, settings);

        let prompt_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Describe a task — Enter to send")
        });
        let commit_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Commit message"));
        let layout = cx.new(|_| ResizableState::default());

        let subscriptions = vec![cx.subscribe_in(&prompt_input, window, Self::on_prompt_event)];

        // Session event pump: drain crossbeam channels into transcripts.
        cx.spawn(async move |this, cx| {
            loop {
                smol::Timer::after(Duration::from_millis(50)).await;
                let alive = this.update(cx, |this: &mut Self, cx| {
                    if this.workspace.pump_events() {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();

        // Git refresh: snapshot on the background pool every 2s.
        cx.spawn(async move |this, cx| {
            loop {
                let Ok(root) =
                    this.read_with(cx, |this: &Self, _| this.workspace.project_root.clone())
                else {
                    break;
                };
                let snap = cx
                    .background_spawn(async move { Workspace::compute_git_snapshot(&root) })
                    .await;
                if this
                    .update(cx, |this: &mut Self, cx| {
                        this.workspace.set_git_snapshot(snap);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
                smol::Timer::after(Duration::from_secs(2)).await;
            }
        })
        .detach();

        Self {
            workspace,
            prompt_input,
            commit_input,
            layout,
            show_settings: false,
            provider_editor: None,
            git_op_status: None,
            transcript_scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    fn on_prompt_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let InputEvent::PressEnter { .. } = event {
            let text = input.read(cx).value().to_string();
            let text = text.trim().to_string();
            if text.is_empty() {
                return;
            }
            self.send_prompt(text, window, cx);
            input.update(cx, |state, cx| state.set_value("", window, cx));
        }
    }

    pub fn send_prompt(&mut self, text: String, _window: &mut Window, cx: &mut Context<Self>) {
        // No session yet: create one named after the prompt.
        if self.workspace.active_session.is_none() {
            let title: String = text.chars().take(32).collect();
            let provider = self.workspace.settings.active_profile();
            if let Err(e) = self.workspace.new_session(&title, provider) {
                log::error!("failed to start session: {e}");
                return;
            }
        }
        if let Some(active) = self.workspace.active_session {
            self.workspace.send_prompt(active, &text);
            self.transcript_scroll.scroll_to_bottom();
            cx.notify();
        }
    }

    pub fn new_session(&mut self, cx: &mut Context<Self>) {
        let n = self.workspace.sessions.len() + 1;
        let provider = self.workspace.settings.active_profile();
        match self.workspace.new_session(&format!("session-{n}"), provider) {
            Ok(_) => cx.notify(),
            Err(e) => log::error!("failed to start session: {e}"),
        }
    }

    pub fn select_session(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.workspace.sessions.len() {
            self.workspace.active_session = Some(index);
            cx.notify();
        }
    }

    pub fn interrupt_active(&mut self, cx: &mut Context<Self>) {
        if let Some(active) = self.workspace.active_session {
            self.workspace.interrupt(active);
            cx.notify();
        }
    }

    /// Point the active session at the settings-selected default
    /// provider/model for its future turns.
    pub fn switch_active_session_profile(&mut self, cx: &mut Context<Self>) {
        if let Some(active) = self.workspace.active_session {
            let profile = self.workspace.settings.active_profile();
            self.workspace.switch_session_profile(active, profile);
            cx.notify();
        }
    }

    // -- git operations -----------------------------------------------------

    pub fn commit_all(&mut self, cx: &mut Context<Self>) {
        let message = self.commit_input.read(cx).value().to_string();
        let message = if message.trim().is_empty() {
            "blurb: checkpoint".to_string()
        } else {
            message
        };
        // Commit the *active session's* checkout when worktree-isolated,
        // otherwise the main project root.
        let root = self
            .workspace
            .active_session
            .and_then(|i| self.workspace.sessions.get(i))
            .map(|s| s.handle.cwd.clone())
            .unwrap_or_else(|| self.workspace.project_root.clone());
        self.run_git_op(cx, move || {
            harness_git::GitRepo::discover(&root)
                .and_then(|r| r.commit_all_default(&message))
                .map(|id| format!("committed {id}"))
                .map_err(|e| format!("commit failed: {e}"))
        });
    }

    /// Merge a session branch (or any local branch) into the project HEAD.
    pub fn merge_branch(&mut self, branch: String, cx: &mut Context<Self>) {
        let root = self.workspace.project_root.clone();
        self.git_op_status = Some(format!("merging {branch}…"));
        cx.notify();
        self.run_git_op(cx, move || {
            match harness_git::GitRepo::discover(&root)
                .and_then(|r| r.merge_branch_into_head(&branch))
            {
                Ok(MergeOutcome::UpToDate) => Ok(format!("{branch}: already up to date")),
                Ok(MergeOutcome::FastForward(id)) => Ok(format!("fast-forwarded to {id}")),
                Ok(MergeOutcome::Merged(id)) => Ok(format!("merged {branch} → {id}")),
                Ok(MergeOutcome::Conflicts(paths)) => Err(format!(
                    "merge stopped — conflicts in: {} (nothing was changed)",
                    paths.join(", ")
                )),
                Err(e) => Err(format!("merge failed: {e}")),
            }
        });
    }

    /// Run a blocking git operation off the UI thread and surface its
    /// outcome message in the git panel.
    fn run_git_op(
        &mut self,
        cx: &mut Context<Self>,
        op: impl FnOnce() -> Result<String, String> + Send + 'static,
    ) {
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { op() }).await;
            let msg = match result {
                Ok(m) => m,
                Err(m) => m,
            };
            let _ = this.update(cx, |this: &mut Self, cx| {
                this.git_op_status = Some(msg);
                cx.notify();
            });
        })
        .detach();
    }

    // -- settings / providers ------------------------------------------------

    pub fn toggle_settings(&mut self, cx: &mut Context<Self>) {
        self.show_settings = !self.show_settings;
        if !self.show_settings {
            self.provider_editor = None;
        }
        cx.notify();
    }

    /// Select a provider (and optionally a model within it) as the default
    /// for new sessions.
    pub fn select_provider_model(
        &mut self,
        provider: usize,
        model: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        self.workspace.settings.select(provider, model);
        let _ = self.workspace.settings.save();
        cx.notify();
    }

    pub fn save_settings(&mut self, cx: &mut Context<Self>) {
        let _ = self.workspace.settings.save();
        cx.notify();
    }
}

impl Render for RootView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let main = h_resizable("root-layout")
            .with_state(&self.layout)
            .child(
                resizable_panel()
                    .size(px(240.))
                    .size_range(px(180.)..px(400.))
                    .child(sidebar::render(self, cx).into_any_element()),
            )
            .child(chat::render(self, window, cx).into_any_element())
            .child(
                resizable_panel()
                    .size(px(320.))
                    .size_range(px(220.)..px(520.))
                    .child(git_panel::render(self, cx).into_any_element()),
            );

        div()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(main)
            .when(self.show_settings, |this| {
                this.child(settings_view::render(self, cx))
            })
    }
}
