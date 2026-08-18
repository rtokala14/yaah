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
use crate::config::{PermissionPolicy, RunProfile};
use crate::context::ContextOptions;
use crate::prompt::build_system_prompt;
use crate::providers;
use crate::tools::builtin_tools;
use crate::types::*;
use crossbeam_channel::{unbounded, Receiver, Sender};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

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
    /// The agent needs approval for a gated tool call. Answer with
    /// `SessionHandle::respond(id, InteractionReply::Permission(..))`.
    PermissionRequest { id: u64, tool: String, summary: String },
    /// The agent asked the user a question. Answer with
    /// `SessionHandle::respond(id, InteractionReply::Answer(..))`.
    AskUser { id: u64, question: String, options: Vec<String> },
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
    /// Durable memory: agent-authored notes + archived compaction summaries.
    #[serde(default)]
    pub memory: SessionMemory,
    /// The session's live todo list.
    #[serde(default)]
    pub todos: Vec<TodoItem>,
}

/// Host-tunable knobs for a session, separate from the model profile.
#[derive(Debug, Clone, Default)]
pub struct SessionOptions {
    /// Restore from + persist to this journal path.
    pub journal: Option<PathBuf>,
    /// Agent-loop turn cap per user message (0 → 1).
    pub max_turns_per_run: u32,
    pub permissions: PermissionPolicy,
    /// Global skills directory (project skills come from
    /// `<workspace>/.blurb/skills` automatically).
    pub skills_global_dir: Option<PathBuf>,
    /// MCP servers to connect for this session (their tools join the
    /// registry as `mcp_<server>_<tool>`, permission-gated).
    pub mcp_servers: Vec<crate::mcp::McpServerConfig>,
    /// Inject a token-budgeted repo map into the (cached) system prompt.
    pub inject_repo_map: bool,
}

/// Bridges the agent's blocking `ask` to the host over channels: emits a
/// SessionEvent carrying a fresh id, then waits (cancel-aware) for the
/// host's `respond` with that id.
struct HostInteraction {
    events: Sender<SessionEvent>,
    replies: Receiver<(u64, InteractionReply)>,
    cancel: CancelToken,
    next_id: AtomicU64,
}

impl InteractionHandler for HostInteraction {
    fn ask(&self, req: InteractionRequest) -> Result<InteractionReply, InteractionError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let event = match req {
            InteractionRequest::Permission { tool, summary } => {
                SessionEvent::PermissionRequest { id, tool, summary }
            }
            InteractionRequest::Question { question, options } => {
                SessionEvent::AskUser { id, question, options }
            }
        };
        if self.events.send(event).is_err() {
            return Err(InteractionError::Unavailable);
        }
        loop {
            if self.cancel.is_cancelled() {
                return Err(InteractionError::Cancelled);
            }
            match self.replies.recv_timeout(Duration::from_millis(100)) {
                Ok((got, reply)) if got == id => return Ok(reply),
                Ok(_) => continue, // stale reply from an earlier prompt
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    return Err(InteractionError::Unavailable)
                }
            }
        }
    }
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
    replies: Sender<(u64, InteractionReply)>,
    cancel: CancelToken,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SessionHandle {
    /// Spawn a session working in `cwd` (typically a git worktree — see
    /// harness-git) against the given resolved provider+model profile.
    /// `SessionOptions` carries persistence, run caps, and permissions.
    pub fn spawn(
        id: u64,
        title: String,
        cwd: PathBuf,
        provider_config: RunProfile,
        options: SessionOptions,
    ) -> Self {
        let (cmd_tx, cmd_rx) = unbounded::<SessionCommand>();
        let (ev_tx, ev_rx) = unbounded::<SessionEvent>();
        let (reply_tx, reply_rx) = unbounded::<(u64, InteractionReply)>();
        let cancel = CancelToken::new();
        let thread_cancel = cancel.clone();
        let thread_cwd = cwd.clone();

        let thread = std::thread::Builder::new()
            .name(format!("session-{id}"))
            .spawn(move || {
                session_thread(
                    thread_cwd,
                    provider_config,
                    options,
                    reply_rx,
                    cmd_rx,
                    ev_tx,
                    thread_cancel,
                )
            })
            .expect("spawn session thread");

        Self {
            id,
            title,
            cwd,
            commands: cmd_tx,
            events: ev_rx,
            replies: reply_tx,
            cancel,
            thread: Some(thread),
        }
    }

    pub fn send(&self, cmd: SessionCommand) {
        if matches!(cmd, SessionCommand::Interrupt) {
            self.cancel.cancel();
        }
        let _ = self.commands.send(cmd);
    }

    /// Answer a pending `PermissionRequest` / `AskUser` event.
    pub fn respond(&self, id: u64, reply: InteractionReply) {
        let _ = self.replies.send((id, reply));
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
    options: SessionOptions,
    replies: Receiver<(u64, InteractionReply)>,
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
    let mut system = build_system_prompt(&cwd);
    // Skills: listed in the (cache-stable) prompt, loaded on demand.
    let skills = crate::skills::discover(&cwd, options.skills_global_dir.as_deref());
    let mut tools = builtin_tools();
    if let Some(section) = crate::skills::prompt_section(&skills) {
        system.push_str("\n\n");
        system.push_str(&section);
        tools.push(Arc::new(crate::skills::SkillTool::new(skills)));
    }
    // MCP servers: connect, adopt their tools, keep clients alive for the
    // session. Failures are surfaced but never fatal.
    let (_mcp_clients, mcp_tools, mcp_errors) =
        crate::mcp::connect_all(&options.mcp_servers, &cwd);
    tools.extend(mcp_tools);
    for e in mcp_errors {
        let _ = events.send(SessionEvent::Agent(AgentEvent::Error(format!(
            "MCP server unavailable — {e}"
        ))));
    }
    // Code index: one build per session; index tools always join (they
    // degrade gracefully), the repo map lands in the cached prefix.
    {
        use harness_index::CodeIndex;
        let index: Arc<dyn CodeIndex> = Arc::new(harness_index::RegexIndex::build(&cwd));
        if options.inject_repo_map {
            if let Ok(map) = index.repo_map(2000) {
                if !map.is_empty() {
                    system.push_str("\n\n# Repo map\nKey definitions by file (ranked by cross-file references; not exhaustive — use symbols/refs/outline/grep for anything else):\n");
                    system.push_str(&map);
                }
            }
        }
        tools.push(Arc::new(crate::tools::index_tools::SymbolsTool::new(Arc::clone(&index))));
        tools.push(Arc::new(crate::tools::index_tools::RefsTool::new(Arc::clone(&index))));
        tools.push(Arc::new(crate::tools::index_tools::OutlineTool::new(index)));
    }
    let effort = provider_config.effort;
    let temperature = provider_config.temperature;
    let max_tokens = provider_config.max_tokens;
    let interaction = Arc::new(HostInteraction {
        events: events.clone(),
        replies,
        cancel: session_cancel.clone(),
        next_id: AtomicU64::new(0),
    });

    // One Agent per session: the transcript persists across user messages.
    let mut agent = Agent::new(
        AgentOptions {
            provider,
            tools,
            system,
            cwd,
            max_turns: options.max_turns_per_run.max(1),
            max_tokens_per_turn: max_tokens,
            effort,
            temperature,
            context: ContextOptions::for_context_window(provider_config.context_window),
            permissions: options.permissions,
            interaction,
        },
        session_cancel.clone(),
    );

    // Restore persisted history, if any. Only usage needs carrying over —
    // turn counts are derived from the (restored) message history itself.
    let journal_path = options.journal;
    let mut session_usage = Usage::default();
    if let Some(path) = &journal_path {
        if let Some(journal) = SessionJournal::load(path) {
            session_usage = journal.usage;
            agent.restore_messages(journal.messages);
            agent.restore_memory(journal.memory);
            agent.restore_todos(journal.todos);
        }
    }
    let persist = |agent: &Agent, usage: Usage, turns: u32| {
        if let Some(path) = &journal_path {
            SessionJournal {
                version: 1,
                messages: agent.messages().to_vec(),
                usage,
                turns,
                memory: agent.memory(),
                todos: agent.todos(),
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
            memory: SessionMemory {
                notes: vec!["tests run headless".into()],
                summaries: vec!["epoch 1 summary".into()],
            },
            todos: vec![TodoItem { text: "ship it".into(), status: TodoStatus::InProgress }],
        };
        journal.save(&path);
        let loaded = SessionJournal::load(&path).unwrap();
        assert_eq!(loaded.messages.len(), 3);
        assert_eq!(loaded.usage.input_tokens, 10);
        assert_eq!(loaded.turns, 1);
        assert_eq!(loaded.memory.notes, vec!["tests run headless".to_string()]);
        assert_eq!(loaded.memory.summaries.len(), 1);
        assert_eq!(loaded.todos.len(), 1);
        assert_eq!(loaded.todos[0].status, TodoStatus::InProgress);

        // Corrupt file → load returns None instead of panicking.
        std::fs::write(&path, "{not json").unwrap();
        assert!(SessionJournal::load(&path).is_none());
    }
}
