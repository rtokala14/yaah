//! Session runner: owns one Agent on a dedicated OS thread and bridges it to
//! a UI (or any host) over crossbeam channels.
//!
//! The UI sends `SessionCommand`s; the session thread emits `SessionEvent`s.
//! The receiver side is cheap to poll from GPUI (try_recv in a frame
//! callback or a background task that forwards into the UI entity).
//!
//! Persistence: give `spawn` a `journal` path and the session thread will
//! seed the agent from it on start (if it exists) and rewrite it after every
//! run and profile switch. The journal carries the full message history plus
//! cumulative usage — everything needed to restore a session across app
//! restarts. Host-side metadata (titles, worktree bindings) lives with the
//! host, not here.

use crate::agent::{Agent, AgentOptions};
use crate::config::RunProfile;
use crate::context::ContextOptions;
use crate::prompt::build_system_prompt;
use crate::providers;
use crate::tools::builtin_tools;
use crate::types::*;
use crossbeam_channel::{unbounded, Receiver, Sender};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug)]
pub enum SessionCommand {
    /// Run a user prompt through the agent loop.
    UserMessage(String),
    /// Cancel the in-flight run (loop stops at the next checkpoint).
    Interrupt,
    /// Swap the provider/model for subsequent turns (transcript is kept;
    /// the prompt-cache prefix resets).
    SetProfile(RunProfile),
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum SessionEvent {
    Agent(AgentEvent),
    RunFinished { final_text: String, turns: u32, usage: Usage, stop_reason: String },
    /// The session switched provider/model (label for display).
    ProfileChanged { label: String },
    Fatal(String),
}

/// On-disk snapshot of a session's conversational state. Written by the
/// session thread after each run; read by hosts to restore transcripts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionJournal {
    pub version: u32,
    pub messages: Vec<AgentMessage>,
    /// Cumulative usage across every run of this session.
    #[serde(default)]
    pub usage: Usage,
    #[serde(default)]
    pub turns: u32,
}

impl SessionJournal {
    pub fn load(path: &std::path::Path) -> Option<SessionJournal> {
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }

    fn save(&self, path: &std::path::Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Write-then-rename so a crash mid-write can't truncate the journal.
        let tmp = path.with_extension("json.tmp");
        if let Ok(text) = serde_json::to_string(self) {
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, path);
            }
        }
    }
}

pub struct SessionHandle {
    pub id: u64,
    pub title: String,
    pub cwd: PathBuf,
    commands: Sender<SessionCommand>,
    pub events: Receiver<SessionEvent>,
    cancel: CancelToken,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SessionHandle {
    /// Spawn a session working in `cwd` (typically a git worktree — see
    /// harness-git) against the given resolved provider+model profile.
    /// When `journal` is set, existing history at that path is restored and
    /// every run is persisted back to it. `max_turns_per_run` bounds one
    /// user message's agent loop (sessions themselves are unbounded — the
    /// context ladder in `context.rs` keeps long ones healthy).
    pub fn spawn(
        id: u64,
        title: String,
        cwd: PathBuf,
        provider_config: RunProfile,
        journal: Option<PathBuf>,
        max_turns_per_run: u32,
    ) -> Self {
        let (cmd_tx, cmd_rx) = unbounded::<SessionCommand>();
        let (ev_tx, ev_rx) = unbounded::<SessionEvent>();
        let cancel = CancelToken::new();
        let thread_cancel = cancel.clone();
        let thread_cwd = cwd.clone();

        let thread = std::thread::Builder::new()
            .name(format!("session-{id}"))
            .spawn(move || {
                session_thread(
                    thread_cwd,
                    provider_config,
                    journal,
                    max_turns_per_run,
                    cmd_rx,
                    ev_tx,
                    thread_cancel,
                )
            })
            .expect("spawn session thread");

        Self { id, title, cwd, commands: cmd_tx, events: ev_rx, cancel, thread: Some(thread) }
    }

    pub fn send(&self, cmd: SessionCommand) {
        if matches!(cmd, SessionCommand::Interrupt) {
            self.cancel.cancel();
        }
        let _ = self.commands.send(cmd);
    }
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
        let _ = self.commands.send(SessionCommand::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn session_thread(
    cwd: PathBuf,
    provider_config: RunProfile,
    journal_path: Option<PathBuf>,
    max_turns_per_run: u32,
    commands: Receiver<SessionCommand>,
    events: Sender<SessionEvent>,
    session_cancel: CancelToken,
) {
    let provider = match providers::build(&provider_config) {
        Ok(p) => p,
        Err(e) => {
            let _ = events.send(SessionEvent::Fatal(e.to_string()));
            return;
        }
    };
    let system = build_system_prompt(&cwd);
    let effort = provider_config.effort;
    let temperature = provider_config.temperature;
    let max_tokens = provider_config.max_tokens;

    // One Agent per session: the transcript persists across user messages.
    let mut agent = Agent::new(
        AgentOptions {
            provider,
            tools: builtin_tools(),
            system,
            cwd,
            max_turns: max_turns_per_run.max(1),
            max_tokens_per_turn: max_tokens,
            effort,
            temperature,
            context: ContextOptions::for_context_window(provider_config.context_window),
        },
        session_cancel.clone(),
    );

    // Restore persisted history, if any. Only usage needs carrying over —
    // turn counts are derived from the (restored) message history itself.
    let mut session_usage = Usage::default();
    if let Some(path) = &journal_path {
        if let Some(journal) = SessionJournal::load(path) {
            session_usage = journal.usage;
            agent.restore_messages(journal.messages);
        }
    }
    let persist = |agent: &Agent, usage: Usage, turns: u32| {
        if let Some(path) = &journal_path {
            SessionJournal {
                version: 1,
                messages: agent.messages().to_vec(),
                usage,
                turns,
            }
            .save(path);
        }
    };

    while let Ok(cmd) = commands.recv() {
        match cmd {
            SessionCommand::Shutdown => break,
            SessionCommand::Interrupt => {
                // The sender cancels the shared token directly (so a run in
                // progress stops); by the time this message is processed the
                // run has returned — re-arm for the next one.
                session_cancel.reset();
            }
            SessionCommand::SetProfile(profile) => match providers::build(&profile) {
                Ok(p) => {
                    agent.set_profile(
                        p,
                        profile.effort,
                        profile.temperature,
                        profile.max_tokens,
                        ContextOptions::for_context_window(profile.context_window),
                    );
                    let _ = events.send(SessionEvent::ProfileChanged { label: profile.label() });
                }
                Err(e) => {
                    let _ = events.send(SessionEvent::Agent(AgentEvent::Error(format!(
                        "profile switch failed (keeping current): {e}"
                    ))));
                }
            },
            SessionCommand::UserMessage(text) => {
                let ev = events.clone();
                let mut emit = |e: AgentEvent| {
                    let _ = ev.send(SessionEvent::Agent(e));
                };
                let result = agent.run(&text, &mut emit, &session_cancel);
                session_usage.add(&result.usage);
                persist(&agent, session_usage, result.turns);
                let _ = events.send(SessionEvent::RunFinished {
                    final_text: result.final_text,
                    turns: result.turns,
                    usage: session_usage,
                    stop_reason: result.stop_reason,
                });
                // If this run was interrupted, re-arm so the session stays usable.
                session_cancel.reset();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_round_trips_and_survives_partial_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested").join("session-1.json");
        let journal = SessionJournal {
            version: 1,
            messages: vec![
                AgentMessage::User {
                    content: vec![UserPart::Text { text: "hi".into() }],
                },
                AgentMessage::Assistant {
                    content: vec![AssistantPart::Text { text: "hello".into() }],
                },
                AgentMessage::SystemNote { content: "note".into() },
            ],
            usage: Usage { input_tokens: 10, output_tokens: 5, ..Default::default() },
            turns: 1,
        };
        journal.save(&path);
        let loaded = SessionJournal::load(&path).unwrap();
        assert_eq!(loaded.messages.len(), 3);
        assert_eq!(loaded.usage.input_tokens, 10);
        assert_eq!(loaded.turns, 1);

        // Corrupt file → load returns None instead of panicking.
        std::fs::write(&path, "{not json").unwrap();
        assert!(SessionJournal::load(&path).is_none());
    }
}
