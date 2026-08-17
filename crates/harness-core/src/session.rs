//! Session runner: owns one Agent on a dedicated OS thread and bridges it to
//! a UI (or any host) over crossbeam channels.
//!
//! The UI sends `SessionCommand`s; the session thread emits `SessionEvent`s.
//! The receiver side is cheap to poll from GPUI (try_recv in a frame
//! callback or a background task that forwards into the UI entity).

use crate::agent::{Agent, AgentOptions};
use crate::config::ProviderConfig;
use crate::context::ContextOptions;
use crate::prompt::build_system_prompt;
use crate::providers;
use crate::tools::builtin_tools;
use crate::types::*;
use crossbeam_channel::{unbounded, Receiver, Sender};
use std::path::PathBuf;

#[derive(Debug)]
pub enum SessionCommand {
    /// Run a user prompt through the agent loop.
    UserMessage(String),
    /// Cancel the in-flight run (loop stops at the next checkpoint).
    Interrupt,
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum SessionEvent {
    Agent(AgentEvent),
    RunFinished { final_text: String, turns: u32, usage: Usage, stop_reason: String },
    Fatal(String),
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
    /// harness-git) against the given provider profile.
    pub fn spawn(id: u64, title: String, cwd: PathBuf, provider_config: ProviderConfig) -> Self {
        let (cmd_tx, cmd_rx) = unbounded::<SessionCommand>();
        let (ev_tx, ev_rx) = unbounded::<SessionEvent>();
        let cancel = CancelToken::new();
        let thread_cancel = cancel.clone();
        let thread_cwd = cwd.clone();

        let thread = std::thread::Builder::new()
            .name(format!("session-{id}"))
            .spawn(move || session_thread(thread_cwd, provider_config, cmd_rx, ev_tx, thread_cancel))
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
    provider_config: ProviderConfig,
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
            max_turns: 80,
            max_tokens_per_turn: max_tokens,
            effort,
            temperature,
            context: ContextOptions::default(),
        },
        session_cancel.clone(),
    );

    while let Ok(cmd) = commands.recv() {
        match cmd {
            SessionCommand::Shutdown => break,
            SessionCommand::Interrupt => {
                // The sender cancels the shared token directly (so a run in
                // progress stops); by the time this message is processed the
                // run has returned — re-arm for the next one.
                session_cancel.reset();
            }
            SessionCommand::UserMessage(text) => {
                let ev = events.clone();
                let mut emit = |e: AgentEvent| {
                    let _ = ev.send(SessionEvent::Agent(e));
                };
                let result = agent.run(&text, &mut emit, &session_cancel);
                let _ = events.send(SessionEvent::RunFinished {
                    final_text: result.final_text,
                    turns: result.turns,
                    usage: result.usage,
                    stop_reason: result.stop_reason,
                });
                // If this run was interrupted, re-arm so the session stays usable.
                session_cancel.reset();
            }
        }
    }
}
