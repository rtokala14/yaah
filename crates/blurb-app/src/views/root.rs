//! Root view: three-pane resizable layout (sidebar | chat | git panel),
//! owns the Workspace model and the two background loops (session event
//! pump, git refresh).

use crate::diff::{parse_patch, DiffLine};
use crate::settings::Settings;
use crate::views::settings_view::{self, ProviderEditor};
use crate::views::{chat, diff_view, git_panel, sidebar};
use crate::workspace::Workspace;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::input::{InputEvent, InputState};
use gpui_component::resizable::{h_resizable, resizable_panel, ResizableState};
use gpui_component::ActiveTheme;
use harness_git::MergeOutcome;
use std::path::PathBuf;
use std::time::Duration;

pub struct DiffView {
    pub path: String,
    pub lines: Vec<DiffLine>,
    pub loading: bool,
}

/// Offered after a successful merge: one click closes the owning session,
/// removes the worktree, and deletes the merged branch.
pub struct MergeCleanup {
    pub branch: String,
    pub worktree: Option<String>,
}

pub struct RootView {
    pub workspace: Workspace,
    pub prompt_input: Entity<InputState>,
    pub commit_input: Entity<InputState>,
    /// Settings overlay: max agent-loop turns per run.
    pub max_turns_input: Entity<InputState>,
    pub layout: Entity<ResizableState>,
    pub show_settings: bool,
    /// In-app provider editor state (Some while editing a provider).
    pub provider_editor: Option<ProviderEditor>,
    /// Outcome of the last git operation (commit/merge), shown in the panel.
    pub git_op_status: Option<String>,
    /// Open per-file diff overlay.
    pub diff_view: Option<DiffView>,
    /// Pending post-merge cleanup offer.
    pub merge_cleanup: Option<MergeCleanup>,
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
        let max_turns = workspace.settings.max_turns_per_run;
        let max_turns_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("250");
            state.set_value(max_turns.to_string(), window, cx);
            state
        });
        let layout = cx.new(|_| ResizableState::default());

        let subscriptions = vec![
            cx.subscribe_in(&prompt_input, window, Self::on_prompt_event),
            cx.subscribe_in(&max_turns_input, window, Self::on_max_turns_event),
        ];

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
            max_turns_input,
            layout,
            show_settings: false,
            provider_editor: None,
            git_op_status: None,
            diff_view: None,
            merge_cleanup: None,
            transcript_scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    fn on_max_turns_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let InputEvent::Change = event {
            if let Ok(n) = input.read(cx).value().trim().parse::<u32>() {
                let n = n.max(1);
                if n != self.workspace.settings.max_turns_per_run {
                    // Applies to sessions spawned from now on.
                    self.workspace.settings.max_turns_per_run = n;
                    let _ = self.workspace.settings.save();
                }
            }
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
    /// On success, offer cleanup of the branch + its worktree.
    pub fn merge_branch(
        &mut self,
        branch: String,
        worktree: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let root = self.workspace.project_root.clone();
        self.git_op_status = Some(format!("merging {branch}…"));
        self.merge_cleanup = None;
        cx.notify();
        let op_branch = branch.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    harness_git::GitRepo::discover(&root)
                        .and_then(|r| r.merge_branch_into_head(&op_branch))
                })
                .await;
            let _ = this.update(cx, |this: &mut Self, cx| {
                let (msg, merged) = match result {
                    Ok(MergeOutcome::UpToDate) => {
                        (format!("{branch}: already up to date"), true)
                    }
                    Ok(MergeOutcome::FastForward(id)) => {
                        (format!("fast-forwarded to {id}"), true)
                    }
                    Ok(MergeOutcome::Merged(id)) => (format!("merged {branch} → {id}"), true),
                    Ok(MergeOutcome::Conflicts(paths)) => (
                        format!(
                            "merge stopped — conflicts in: {} (nothing was changed)",
                            paths.join(", ")
                        ),
                        false,
                    ),
                    Err(e) => (format!("merge failed: {e}"), false),
                };
                if merged {
                    this.merge_cleanup = Some(MergeCleanup { branch, worktree });
                }
                this.git_op_status = Some(msg);
                cx.notify();
            });
        })
        .detach();
    }

    /// Execute a pending merge-cleanup offer: close the owning session,
    /// remove the worktree, delete the merged branch.
    pub fn perform_merge_cleanup(&mut self, cx: &mut Context<Self>) {
        let Some(cleanup) = self.merge_cleanup.take() else { return };
        if let Some(name) = &cleanup.worktree {
            if let Some(i) = self.workspace.worktree_sessions().get(name).copied() {
                self.workspace.close_session(i, true);
            } else {
                // Worktree without a live session: remove it directly by
                // going through git (it shows up in the next snapshot).
                let root = self.workspace.project_root.clone();
                let wt = name.clone();
                cx.background_spawn(async move {
                    let _ = harness_git::GitRepo::discover(&root)
                        .and_then(|r| r.remove_worktree(&wt));
                })
                .detach();
            }
        }
        let branch = cleanup.branch.clone();
        let root = self.workspace.project_root.clone();
        self.run_git_op(cx, move || {
            harness_git::GitRepo::discover(&root)
                .and_then(|r| r.delete_branch(&branch))
                .map(|_| format!("cleaned up {branch}"))
                .map_err(|e| format!("branch delete failed: {e}"))
        });
        cx.notify();
    }

    pub fn dismiss_merge_cleanup(&mut self, cx: &mut Context<Self>) {
        self.merge_cleanup = None;
        cx.notify();
    }

    /// Open the diff overlay for one changed file (loads off-thread).
    pub fn open_file_diff(&mut self, path: String, cx: &mut Context<Self>) {
        let root = self.workspace.project_root.clone();
        self.diff_view =
            Some(DiffView { path: path.clone(), lines: Vec::new(), loading: true });
        cx.notify();
        let file = path.clone();
        cx.spawn(async move |this, cx| {
            let patch = cx
                .background_spawn(async move {
                    harness_git::GitRepo::discover(&root)
                        .and_then(|r| r.diff_patch_file(&file))
                        .map_err(|e| e.to_string())
                })
                .await;
            let _ = this.update(cx, |this: &mut Self, cx| {
                if let Some(dv) = &mut this.diff_view {
                    if dv.path == path {
                        dv.loading = false;
                        dv.lines = match patch {
                            Ok(p) if p.trim().is_empty() => {
                                parse_patch("(no uncommitted changes in this file)")
                            }
                            Ok(p) => parse_patch(&p),
                            Err(e) => parse_patch(&format!("diff failed: {e}")),
                        };
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    pub fn close_diff_view(&mut self, cx: &mut Context<Self>) {
        self.diff_view = None;
        cx.notify();
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
            .when(self.diff_view.is_some(), |this| {
                this.child(diff_view::render(self, cx))
            })
    }
}
