//! Workspace model: one open project (a git repo), its sessions, and cached
//! git state. GPUI-free; the root view owns one of these and re-renders from
//! it. All methods are cheap except `refresh_git`, which the UI calls from a
//! background task.

use crate::settings::Settings;
use crate::transcript::Transcript;
use harness_core::config::ProviderConfig;
use harness_core::{SessionCommand, SessionHandle};
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

pub struct Workspace {
    pub settings: Settings,
    pub project_root: PathBuf,
    pub sessions: Vec<SessionState>,
    pub active_session: Option<usize>,
    pub git: Option<RepoSnapshot>,
    pub git_error: Option<String>,
    next_session_id: u64,
    worktrees: WorktreeManager,
}

impl Workspace {
    pub fn open(project_root: PathBuf, mut settings: Settings) -> Self {
        settings.remember_project(project_root.clone());
        let _ = settings.save();
        let worktrees = WorktreeManager::new(project_root.clone());
        Self {
            settings,
            project_root,
            sessions: Vec::new(),
            active_session: None,
            git: None,
            git_error: None,
            next_session_id: 1,
            worktrees,
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
    pub fn new_session(&mut self, title: &str, provider: ProviderConfig) -> Result<usize, String> {
        let (cwd, worktree) = if self.settings.sessions_use_worktrees {
            match self.worktrees.create_session_worktree(title) {
                Ok(wt) => (wt.path.clone(), Some(wt.name)),
                Err(e) => return Err(format!("worktree: {e}")),
            }
        } else {
            (self.project_root.clone(), None)
        };

        let id = self.next_session_id;
        self.next_session_id += 1;
        let provider_label = format!("{} · {}", provider.name, provider.model);
        let handle = SessionHandle::spawn(id, title.to_string(), cwd, provider);
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

    /// Drain pending events from every session into its transcript.
    /// Returns true if anything changed (the UI should re-render).
    pub fn pump_events(&mut self) -> bool {
        let mut changed = false;
        for s in &mut self.sessions {
            while let Ok(ev) = s.handle.events.try_recv() {
                s.transcript.apply(&ev);
                changed = true;
            }
        }
        changed
    }

    pub fn close_session(&mut self, index: usize, remove_worktree: bool) {
        if index >= self.sessions.len() {
            return;
        }
        let state = self.sessions.remove(index);
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

    /// Sessions grouped for the sidebar: (index, title, running, provider).
    pub fn session_rows(&self) -> Vec<(usize, String, bool, String)> {
        self.sessions
            .iter()
            .enumerate()
            .map(|(i, s)| {
                (i, s.handle.title.clone(), s.transcript.running, s.provider_label.clone())
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
