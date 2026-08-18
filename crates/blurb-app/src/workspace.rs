//! Workspace model: one open project (a git repo), its sessions, and cached
//! git state. GPUI-free; the root view owns one of these and re-renders from
//! it. All methods are cheap except `refresh_git`, which the UI calls from a
//! background task.
//!
//! Sessions persist: metadata (titles, worktree bindings) in the project
//! store, message history in per-session journals written by the session
//! threads. `open` restores everything.

use crate::persist::{ProjectStore, SessionMeta};
use crate::settings::Settings;
use crate::transcript::Transcript;
use harness_core::config::RunProfile;
use harness_core::types::{InteractionReply, PermissionDecision};
use harness_core::{SessionCommand, SessionEvent, SessionHandle, SessionJournal, SessionOptions};
use harness_git::{GitRepo, RepoSnapshot, WorktreeManager};
use std::collections::HashMap;
use std::path::PathBuf;

pub struct SessionState {
    pub handle: SessionHandle,
    pub transcript: Transcript,
    /// Worktree name when this session is isolated (sessions_use_worktrees).
    pub worktree: Option<String>,
    pub provider_label: String,
}

/// One sidebar row (kept a struct — it keeps growing).
pub struct SessionRow {
    pub index: usize,
    pub title: String,
    pub running: bool,
    pub provider_label: String,
    pub total_tokens: u64,
}

pub struct Workspace {
    pub settings: Settings,
    pub project_root: PathBuf,
    pub sessions: Vec<SessionState>,
    pub active_session: Option<usize>,
    pub git: Option<RepoSnapshot>,
    pub git_error: Option<String>,
    store: ProjectStore,
    worktrees: WorktreeManager,
}

impl Workspace {
    pub fn open(project_root: PathBuf, mut settings: Settings) -> Self {
        settings.remember_project(project_root.clone());
        let _ = settings.save();
        let worktrees = WorktreeManager::new(project_root.clone());
        let store = ProjectStore::open(&project_root);
        let mut ws = Self {
            settings,
            project_root,
            sessions: Vec::new(),
            active_session: None,
            git: None,
            git_error: None,
            store,
            worktrees,
        };
        ws.restore_sessions();
        ws
    }

    /// Re-spawn every persisted session: agent history from its journal,
    /// transcript rebuilt from the same messages. A session whose checkout
    /// vanished (worktree removed outside the app) falls back to the
    /// project root.
    fn restore_sessions(&mut self) {
        let metas: Vec<SessionMeta> = self.store.index.sessions.clone();
        for meta in metas {
            let (cwd, worktree) = if meta.cwd.exists() {
                (meta.cwd.clone(), meta.worktree.clone())
            } else {
                (self.project_root.clone(), None)
            };
            let profile = self
                .settings
                .profile_by_label(&meta.provider_label)
                .unwrap_or_else(|| self.settings.active_profile());
            let journal_path = self.store.journal_path(meta.id);
            let transcript = SessionJournal::load(&journal_path)
                .map(|j| {
                    let mut t = Transcript::from_messages(&j.messages, j.usage, j.turns);
                    t.memory_count = j.memory.notes.len();
                    t.todos = j.todos;
                    t
                })
                .unwrap_or_default();
            let handle = SessionHandle::spawn(
                meta.id,
                meta.title.clone(),
                cwd,
                profile.clone(),
                self.session_options(Some(journal_path)),
            );
            self.sessions.push(SessionState {
                handle,
                transcript,
                worktree,
                provider_label: profile.label(),
            });
        }
        if !self.sessions.is_empty() {
            self.active_session = Some(self.sessions.len() - 1);
        }
    }

    fn session_options(&self, journal: Option<std::path::PathBuf>) -> SessionOptions {
        SessionOptions {
            journal,
            max_turns_per_run: self.settings.max_turns_per_run,
            permissions: self.settings.permissions.clone(),
            skills_global_dir: dirs::config_dir().map(|d| d.join("blurb").join("skills")),
            mcp_servers: self.settings.mcp_servers.clone(),
            inject_repo_map: self.settings.inject_repo_map,
            hooks: self.settings.hooks.clone(),
        }
    }

    /// Snapshot git state (runs libgit2; call from a background task).
    pub fn compute_git_snapshot(root: &PathBuf) -> Result<RepoSnapshot, String> {
        GitRepo::discover(root).and_then(|r| r.snapshot()).map_err(|e| e.to_string())
    }

    pub fn set_git_snapshot(&mut self, snap: Result<RepoSnapshot, String>) {
        match snap {
            Ok(s) => {
                self.git = Some(s);
                self.git_error = None;
            }
            Err(e) => self.git_error = Some(e),
        }
    }

    /// Start a session. When worktree isolation is on, the session gets its
    /// own checkout + `blurb/<slug>` branch; otherwise it runs in the main
    /// checkout.
    pub fn new_session(&mut self, title: &str, provider: RunProfile) -> Result<usize, String> {
        let (cwd, worktree) = if self.settings.sessions_use_worktrees {
            match self.worktrees.create_session_worktree(title) {
                Ok(wt) => (wt.path.clone(), Some(wt.name)),
                Err(e) => return Err(format!("worktree: {e}")),
            }
        } else {
            (self.project_root.clone(), None)
        };

        let id = self.store.allocate_id();
        let provider_label = provider.label();
        self.store.register(SessionMeta {
            id,
            title: title.to_string(),
            provider_label: provider_label.clone(),
            worktree: worktree.clone(),
            cwd: cwd.clone(),
        });
        self.store.save();
        let journal = Some(self.store.journal_path(id));
        let handle =
            SessionHandle::spawn(id, title.to_string(), cwd, provider, self.session_options(journal));
        self.sessions.push(SessionState {
            handle,
            transcript: Transcript::default(),
            worktree,
            provider_label,
        });
        let index = self.sessions.len() - 1;
        self.active_session = Some(index);
        Ok(index)
    }

    pub fn send_prompt(&mut self, session: usize, prompt: &str) {
        if let Some(s) = self.sessions.get_mut(session) {
            s.transcript.push_user(prompt);
            s.handle.send(SessionCommand::UserMessage(prompt.to_string()));
        }
    }

    pub fn interrupt(&mut self, session: usize) {
        if let Some(s) = self.sessions.get(session) {
            s.handle.send(SessionCommand::Interrupt);
        }
    }

    /// Swap the provider/model for a running session's future turns. The
    /// label updates when the session confirms (`ProfileChanged`).
    pub fn switch_session_profile(&mut self, session: usize, profile: RunProfile) {
        if let Some(s) = self.sessions.get(session) {
            s.handle.send(SessionCommand::SetProfile(profile));
        }
    }

    /// Answer the agent's pending question. Returns true if delivered.
    pub fn answer_question(&mut self, session: usize, id: u64, text: &str) -> bool {
        let Some(s) = self.sessions.get_mut(session) else { return false };
        s.handle.respond(id, InteractionReply::Answer(text.to_string()));
        s.transcript.record_answer(id, text);
        true
    }

    /// Decide a pending permission request. Returns the (tool, summary)
    /// pair so the caller can persist AllowAlways decisions into settings.
    pub fn respond_permission(
        &mut self,
        session: usize,
        id: u64,
        decision: PermissionDecision,
    ) -> Option<(String, String)> {
        let s = self.sessions.get_mut(session)?;
        let subject = s.transcript.permission_subject(id);
        s.handle.respond(id, InteractionReply::Permission(decision));
        s.transcript.record_decision(
            id,
            match decision {
                PermissionDecision::Allow => "allowed",
                PermissionDecision::AllowAlways => "always allowed",
                PermissionDecision::Deny => "denied",
            },
        );
        subject
    }

    /// The active question awaiting an answer in a session, if any.
    pub fn pending_question(&self, session: usize) -> Option<u64> {
        self.sessions.get(session).and_then(|s| s.transcript.pending_question)
    }

    /// Rename a session (auto-titling or user edit) in the handle and the
    /// persisted metadata.
    pub fn rename_session(&mut self, session: usize, title: &str) {
        if let Some(s) = self.sessions.get_mut(session) {
            s.handle.title = title.to_string();
            self.store.update_title(s.handle.id, title);
            self.store.save();
        }
    }

    /// Drain pending events from every session into its transcript.
    /// Returns true if anything changed (the UI should re-render).
    pub fn pump_events(&mut self) -> bool {
        let mut changed = false;
        let mut label_updates: Vec<(u64, String)> = Vec::new();
        for s in &mut self.sessions {
            while let Ok(ev) = s.handle.events.try_recv() {
                if let SessionEvent::ProfileChanged { label } = &ev {
                    s.provider_label = label.clone();
                    label_updates.push((s.handle.id, label.clone()));
                }
                s.transcript.apply(&ev);
                changed = true;
            }
        }
        if !label_updates.is_empty() {
            for (id, label) in label_updates {
                self.store.update_provider_label(id, &label);
            }
            self.store.save();
        }
        changed
    }

    pub fn close_session(&mut self, index: usize, remove_worktree: bool) {
        if index >= self.sessions.len() {
            return;
        }
        let state = self.sessions.remove(index);
        self.store.remove(state.handle.id);
        self.store.save();
        if remove_worktree {
            if let Some(name) = &state.worktree {
                let _ = self.worktrees.remove_session_worktree(name);
            }
        }
        drop(state); // joins the session thread
        self.active_session = match self.sessions.len() {
            0 => None,
            n => Some(index.min(n - 1)),
        };
    }

    pub fn session_rows(&self) -> Vec<SessionRow> {
        self.sessions
            .iter()
            .enumerate()
            .map(|(i, s)| SessionRow {
                index: i,
                title: s.handle.title.clone(),
                running: s.transcript.running,
                provider_label: s.provider_label.clone(),
                total_tokens: s.transcript.usage.input_tokens
                    + s.transcript.usage.cache_read_tokens
                    + s.transcript.usage.output_tokens,
            })
            .collect()
    }

    /// Map worktree name -> session index, for the git panel.
    pub fn worktree_sessions(&self) -> HashMap<String, usize> {
        self.sessions
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.worktree.clone().map(|w| (w, i)))
            .collect()
    }
}
