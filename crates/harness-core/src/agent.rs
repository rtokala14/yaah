//! The agent loop: one flat loop, no planner/executor split.
//!
//! Per turn: stream the model → if it called tools, execute them (adjacent
//! read-only calls in parallel) → append results → manage context → repeat.
//! Evidence-grounded completion: if the model claims done after mutating
//! files without running anything, inject a one-time system-note nudge and
//! continue (deterministic harness gate; no prompt scaffolding).

use crate::config::PermissionPolicy;
use crate::context::{self, ContextOptions};
use crate::types::*;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

const MUTATING_TOOLS: &[&str] = &["write", "edit"];
const VERIFYING_TOOLS: &[&str] = &["bash"];
/// Tools that require human approval when the policy says ask.
const GATED_TOOLS: &[&str] = &["write", "edit", "bash"];

pub struct AgentOptions {
    pub provider: Arc<dyn Provider>,
    pub tools: Vec<Arc<dyn Tool>>,
    pub system: String,
    pub cwd: std::path::PathBuf,
    pub max_turns: u32,
    pub max_tokens_per_turn: u32,
    pub effort: Option<Effort>,
    pub temperature: Option<f64>,
    pub context: ContextOptions,
    pub permissions: PermissionPolicy,
    pub interaction: Arc<dyn InteractionHandler>,
    pub hooks: Vec<crate::hooks::HookConfig>,
}

pub struct AgentResult {
    pub final_text: String,
    pub turns: u32,
    pub usage: Usage,
    pub stop_reason: String,
}

pub struct Agent {
    opts: AgentOptions,
    messages: Vec<AgentMessage>,
    tools_by_name: HashMap<String, Arc<dyn Tool>>,
    tool_ctx: Arc<ToolContext>,
    /// Notes already announced via `MemoryNote` events (memory itself lives
    /// in `tool_ctx.memory`, the single source of truth).
    announced_notes: usize,
    /// AllowAlways decisions made during this session (the host persists
    /// them into settings for future sessions; these cover the current one).
    session_allow_edits: bool,
    session_allowed_bash: Vec<String>,
    session_allowed_tools: Vec<String>,
    unverified_mutation: bool,
    verify_nudge_used: bool,
}

impl Agent {
    pub fn new(opts: AgentOptions, cancel: CancelToken) -> Self {
        let tools_by_name = opts
            .tools
            .iter()
            .map(|t| (t.def().name.clone(), Arc::clone(t)))
            .collect();
        let mut tool_ctx =
            ToolContext::with_interaction(opts.cwd.clone(), cancel, Arc::clone(&opts.interaction));
        tool_ctx.hooks = opts.hooks.clone();
        let tool_ctx = Arc::new(tool_ctx);
        // Install the nested-agent runner (primary agents only — nested
        // toolsets exclude the subagent tool, so recursion cannot occur).
        *tool_ctx.subagent.lock().unwrap() = Some(Arc::new(NestedRunner {
            provider: Arc::clone(&opts.provider),
            tools: nested_toolset(&opts.tools),
            system: opts.system.clone(),
            cwd: opts.cwd.clone(),
            context: opts.context.clone(),
            max_tokens_per_turn: opts.max_tokens_per_turn,
            effort: opts.effort,
            temperature: opts.temperature,
        }));
        Self {
            opts,
            messages: Vec::new(),
            tools_by_name,
            tool_ctx,
            announced_notes: 0,
            session_allow_edits: false,
            session_allowed_bash: Vec::new(),
            session_allowed_tools: Vec::new(),
            unverified_mutation: false,
            verify_nudge_used: false,
        }
    }

    pub fn messages(&self) -> &[AgentMessage] {
        &self.messages
    }

    /// Seed the transcript from a persisted journal (session restore).
    /// Replaces any existing history; the next run continues from it.
    pub fn restore_messages(&mut self, messages: Vec<AgentMessage>) {
        self.messages = messages;
    }

    pub fn memory(&self) -> SessionMemory {
        self.tool_ctx.memory.lock().unwrap().clone()
    }

    pub fn todos(&self) -> Vec<TodoItem> {
        self.tool_ctx.todos.lock().unwrap().clone()
    }

    pub fn restore_todos(&mut self, todos: Vec<TodoItem>) {
        *self.tool_ctx.todos.lock().unwrap() = todos;
    }

    /// Seed durable memory from a persisted journal (session restore).
    /// Restored notes are not re-announced as events.
    pub fn restore_memory(&mut self, memory: SessionMemory) {
        self.announced_notes = memory.notes.len();
        *self.tool_ctx.memory.lock().unwrap() = memory;
    }

    /// Swap the provider/model for subsequent turns. The transcript is kept
    /// (the new model sees the full history); the prompt-cache prefix resets,
    /// which the caller should surface to the user. The context budget
    /// follows the new model's window.
    pub fn set_profile(
        &mut self,
        provider: Arc<dyn Provider>,
        effort: Option<Effort>,
        temperature: Option<f64>,
        max_tokens_per_turn: u32,
        context: ContextOptions,
    ) {
        self.opts.provider = provider;
        self.opts.effort = effort;
        self.opts.temperature = temperature;
        self.opts.max_tokens_per_turn = max_tokens_per_turn;
        self.opts.context = context;
    }

    /// Inject harness-side steering between turns without touching the
    /// system prompt (cache-safe).
    pub fn add_system_note(&mut self, content: impl Into<String>) {
        self.messages.push(AgentMessage::SystemNote { content: content.into() });
    }

    pub fn run(
        &mut self,
        user_input: &str,
        emit: &mut dyn FnMut(AgentEvent),
        cancel: &CancelToken,
    ) -> AgentResult {
        self.messages.push(AgentMessage::User {
            content: vec![UserPart::Text { text: user_input.to_string() }],
        });

        let mut totals = Usage::default();
        let mut final_text = String::new();
        let mut stop = String::from("max_turns");
        let tool_defs: Vec<ToolDef> =
            self.opts.tools.iter().map(|t| t.def().clone()).collect();

        for turn in 1..=self.opts.max_turns {
            if cancel.is_cancelled() {
                stop = "cancelled".into();
                break;
            }
            self.manage_context(emit, cancel);
            emit(AgentEvent::TurnStart {
                turn,
                context_tokens: context::estimate_tokens(&self.messages),
                budget_tokens: self.opts.context.budget_tokens,
            });

            let req = ProviderRequest {
                system: self.opts.system.clone(),
                messages: self.messages.clone(),
                tools: tool_defs.clone(),
                max_tokens: self.opts.max_tokens_per_turn,
                effort: self.opts.effort,
                temperature: self.opts.temperature,
            };
            let mut relay = |d: StreamDelta| match d {
                StreamDelta::Text(t) => emit(AgentEvent::TextDelta(t)),
                StreamDelta::Thinking(t) => emit(AgentEvent::ThinkingDelta(t)),
                StreamDelta::ToolCallStart(_) => {}
            };
            let turn_result = match self.opts.provider.stream(&req, &mut relay, cancel) {
                Ok(r) => r,
                Err(ProviderError::Cancelled) => {
                    stop = "cancelled".into();
                    break;
                }
                Err(e) => {
                    stop = "error".into();
                    final_text = format!("provider error: {e}");
                    emit(AgentEvent::Error(final_text.clone()));
                    break;
                }
            };

            totals.add(&turn_result.usage);
            let text: String = turn_result
                .content
                .iter()
                .filter_map(|p| match p {
                    AssistantPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            if !text.is_empty() {
                final_text = text;
            }
            let calls: Vec<ToolCallPart> = turn_result
                .content
                .iter()
                .filter_map(|p| match p {
                    AssistantPart::ToolCall(c) => Some(c.clone()),
                    _ => None,
                })
                .collect();
            self.messages.push(AgentMessage::Assistant { content: turn_result.content });
            emit(AgentEvent::TurnEnd {
                stop_reason: turn_result.stop_reason,
                usage: turn_result.usage,
            });

            if turn_result.stop_reason != StopReason::ToolUse || calls.is_empty() {
                // Evidence-grounded completion gate.
                if turn_result.stop_reason == StopReason::EndTurn
                    && self.unverified_mutation
                    && !self.verify_nudge_used
                {
                    self.verify_nudge_used = true;
                    self.add_system_note(
                        "You modified files but have not run any command since the last modification. Verify the change with the project's own signals (tests, typechecker, build) and report the result — or state explicitly why verification is not possible — before finishing.",
                    );
                    continue;
                }
                stop = format!("{:?}", turn_result.stop_reason).to_lowercase();
                break;
            }

            let results = self.execute_tool_calls(&calls, emit);
            self.messages.push(AgentMessage::User {
                content: results.into_iter().map(UserPart::ToolResult).collect(),
            });
        }

        emit(AgentEvent::Done { reason: stop.clone(), final_text: final_text.clone() });
        AgentResult {
            final_text,
            turns: self
                .messages
                .iter()
                .filter(|m| matches!(m, AgentMessage::Assistant { .. }))
                .count() as u32,
            usage: totals,
            stop_reason: stop,
        }
    }

    /// Adjacent read-only calls run in parallel (scoped threads); a mutating
    /// call is a barrier executed alone, in order.
    fn execute_tool_calls(
        &mut self,
        calls: &[ToolCallPart],
        emit: &mut dyn FnMut(AgentEvent),
    ) -> Vec<ToolResultPart> {
        let mut results: Vec<Option<ToolResultPart>> = vec![None; calls.len()];
        let mut i = 0;
        while i < calls.len() {
            let read_only = |c: &ToolCallPart| {
                self.tools_by_name.get(&c.name).map(|t| t.read_only()).unwrap_or(false)
            };
            if read_only(&calls[i]) {
                let mut j = i;
                while j < calls.len() && read_only(&calls[j]) {
                    j += 1;
                }
                let batch = &calls[i..j];
                for c in batch {
                    emit(AgentEvent::ToolStart { name: c.name.clone(), input: c.input.clone() });
                }
                let batch_results: Vec<(usize, ToolResultPart, u64)> =
                    std::thread::scope(|scope| {
                        let handles: Vec<_> = batch
                            .iter()
                            .enumerate()
                            .map(|(k, call)| {
                                let tool = self.tools_by_name.get(&call.name).cloned();
                                let ctx = Arc::clone(&self.tool_ctx);
                                scope.spawn(move || {
                                    let started = Instant::now();
                                    let r = run_tool(tool, call, &ctx);
                                    (k, r, started.elapsed().as_millis() as u64)
                                })
                            })
                            .collect();
                        handles.into_iter().filter_map(|h| h.join().ok()).collect()
                    });
                for (k, r, ms) in batch_results {
                    emit(AgentEvent::ToolEnd {
                        name: r.tool_name.clone(),
                        output_preview: preview(&r.content),
                        is_error: r.is_error,
                        duration_ms: ms,
                    });
                    results[i + k] = Some(r);
                }
                i = j;
            } else {
                let call = &calls[i];
                emit(AgentEvent::ToolStart { name: call.name.clone(), input: call.input.clone() });
                let started = Instant::now();
                // Human gate: mutating tools need approval under the policy.
                if let Some(denial) = self.permission_gate(call) {
                    emit(AgentEvent::ToolEnd {
                        name: call.name.clone(),
                        output_preview: preview(&denial.content),
                        is_error: true,
                        duration_ms: started.elapsed().as_millis() as u64,
                    });
                    results[i] = Some(denial);
                    i += 1;
                    continue;
                }
                let tool = self.tools_by_name.get(&call.name).cloned();
                let r = run_tool(tool, call, &self.tool_ctx);
                if MUTATING_TOOLS.contains(&call.name.as_str()) && !r.is_error {
                    self.unverified_mutation = true;
                } else if VERIFYING_TOOLS.contains(&call.name.as_str()) {
                    self.unverified_mutation = false;
                }
                emit(AgentEvent::ToolEnd {
                    name: call.name.clone(),
                    output_preview: preview(&r.content),
                    is_error: r.is_error,
                    duration_ms: started.elapsed().as_millis() as u64,
                });
                results[i] = Some(r);
                i += 1;
            }
        }
        // Announce notes recorded by `remember` calls in this batch.
        {
            let memory = self.tool_ctx.memory.lock().unwrap();
            for note in memory.notes.iter().skip(self.announced_notes) {
                emit(AgentEvent::MemoryNote { text: note.clone() });
            }
            self.announced_notes = memory.notes.len();
        }
        // Publish the todo list if this batch rewrote it.
        if calls.iter().any(|c| c.name == "todo_write") {
            emit(AgentEvent::TodosUpdated { todos: self.tool_ctx.todos.lock().unwrap().clone() });
        }
        results.into_iter().flatten().collect()
    }

    /// Returns Some(denial result) when the call may not run. Read-only
    /// tools never reach this (they execute in the parallel branch).
    fn permission_gate(&mut self, call: &ToolCallPart) -> Option<ToolResultPart> {
        let is_mcp = call.name.starts_with("mcp_");
        if !self.opts.permissions.ask
            || (!GATED_TOOLS.contains(&call.name.as_str()) && !is_mcp)
        {
            return None;
        }
        let is_edit = MUTATING_TOOLS.contains(&call.name.as_str());
        let summary = if call.name == "bash" {
            call.input.get("command").and_then(|v| v.as_str()).unwrap_or("").to_string()
        } else if is_mcp {
            serde_json::to_string(&call.input).unwrap_or_default().chars().take(160).collect()
        } else {
            call.input.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string()
        };
        if is_edit && (self.opts.permissions.allow_edits || self.session_allow_edits) {
            return None;
        }
        if call.name == "bash" {
            let program = summary.split_whitespace().next().unwrap_or("").to_string();
            if self.opts.permissions.bash_allowed(&summary)
                || self.session_allowed_bash.contains(&program)
            {
                return None;
            }
        }
        if is_mcp
            && (self.opts.permissions.allowed_tools.contains(&call.name)
                || self.session_allowed_tools.contains(&call.name))
        {
            return None;
        }

        let deny = |reason: &str| {
            Some(ToolResultPart {
                tool_call_id: call.id.clone(),
                tool_name: call.name.clone(),
                content: reason.to_string(),
                is_error: true,
            })
        };
        match self.opts.interaction.ask(InteractionRequest::Permission {
            tool: call.name.clone(),
            summary: summary.clone(),
        }) {
            Ok(InteractionReply::Permission(PermissionDecision::Allow)) => None,
            Ok(InteractionReply::Permission(PermissionDecision::AllowAlways)) => {
                if is_edit {
                    self.session_allow_edits = true;
                } else if is_mcp {
                    self.session_allowed_tools.push(call.name.clone());
                } else if let Some(program) = summary.split_whitespace().next() {
                    self.session_allowed_bash.push(program.to_string());
                }
                None
            }
            Ok(InteractionReply::Permission(PermissionDecision::Deny)) => deny(
                "The user denied permission for this action. Do not retry it unchanged; adjust the approach or ask what they would prefer.",
            ),
            Ok(_) => deny("host returned a mismatched reply; action not run"),
            Err(InteractionError::Cancelled) => deny("interrupted before approval"),
            Err(InteractionError::Unavailable) => deny(
                "no interactive user is attached to approve this action; it was not run",
            ),
        }
    }

    /// Escalation ladder: prune stale → strip stale thinking → force-prune →
    /// compact. Each step runs only if the previous ones left the transcript
    /// over threshold, so cheap lossless steps absorb most of the pressure
    /// and full cache-rebuilding compaction stays rare even in 250-turn
    /// sessions.
    fn manage_context(&mut self, emit: &mut dyn FnMut(AgentEvent), cancel: &CancelToken) {
        let opts = self.opts.context.clone();
        let over = |tokens: usize, at: f32| tokens as f32 > opts.budget_tokens as f32 * at;
        let mut tokens = context::estimate_tokens(&self.messages);

        if over(tokens, opts.prune_at) {
            let pruned = context::prune_tool_results(&mut self.messages, &opts);
            let stripped =
                context::strip_stale_thinking(&mut self.messages, opts.keep_recent_turns);
            if pruned + stripped > 0 {
                emit(AgentEvent::Pruned { count: pruned + stripped });
                tokens = context::estimate_tokens(&self.messages);
            }
        }

        if over(tokens, opts.compact_at) {
            // Before paying for a summary and a full cache rebuild, take
            // everything the lossless steps can still give.
            let pruned = context::force_prune_tool_results(&mut self.messages, &opts);
            let stripped = context::strip_stale_thinking(&mut self.messages, 1);
            if pruned + stripped > 0 {
                emit(AgentEvent::Pruned { count: pruned + stripped });
                tokens = context::estimate_tokens(&self.messages);
            }
        }

        if over(tokens, opts.compact_at) {
            emit(AgentEvent::Compaction { before_tokens: tokens });
            let memory = self.memory();
            match context::compact(
                self.opts.provider.as_ref(),
                &self.opts.system,
                &self.messages,
                &memory,
                &opts,
                cancel,
            ) {
                Ok((replacement, summary)) => {
                    self.messages = replacement;
                    if !summary.is_empty() {
                        self.tool_ctx.memory.lock().unwrap().summaries.push(summary);
                    }
                }
                Err(e) => emit(AgentEvent::Error(format!("compaction failed: {e}"))),
            }
        }
    }
}

/// Tools a nested explorer may use: read-only, minus the ones that make no
/// sense one level down (spawning further subagents, writing the parent's
/// plan or memory, blocking on the user).
pub fn nested_toolset(tools: &[Arc<dyn Tool>]) -> Vec<Arc<dyn Tool>> {
    const EXCLUDED: &[&str] = &["subagent", "todo_write", "remember"];
    tools
        .iter()
        .filter(|t| t.read_only() && !EXCLUDED.contains(&t.def().name.as_str()))
        .cloned()
        .collect()
}

/// Runs one nested explorer agent to completion, silently (no event
/// stream); only the final report crosses back.
struct NestedRunner {
    provider: Arc<dyn Provider>,
    tools: Vec<Arc<dyn Tool>>,
    system: String,
    cwd: std::path::PathBuf,
    context: ContextOptions,
    max_tokens_per_turn: u32,
    effort: Option<Effort>,
    temperature: Option<f64>,
}

const SUBAGENT_MAX_TURNS: u32 = 25;

impl SubagentRunner for NestedRunner {
    fn run(&self, task: &str, cancel: &CancelToken) -> Result<String, String> {
        let mut system = self.system.clone();
        system.push_str(
            "\n\n# Subagent\nYou are a read-only explorer subagent. Investigate the task and REPORT — you cannot modify files or run commands. Your final message is delivered verbatim to the primary agent: lead with the answer, cite exact paths and identifiers, and keep it dense.",
        );
        let mut agent = Agent::new(
            AgentOptions {
                provider: Arc::clone(&self.provider),
                tools: self.tools.clone(),
                system,
                cwd: self.cwd.clone(),
                max_turns: SUBAGENT_MAX_TURNS,
                max_tokens_per_turn: self.max_tokens_per_turn,
                effort: self.effort,
                temperature: self.temperature,
                context: self.context.clone(),
                permissions: crate::config::PermissionPolicy {
                    ask: false, // toolset is read-only; nothing to gate
                    ..Default::default()
                },
                interaction: Arc::new(NoInteraction),
                hooks: Vec::new(), // hooks target mutations; explorers have none
            },
            cancel.clone(),
        );
        let result = agent.run(task, &mut |_| {}, cancel);
        if result.stop_reason == "cancelled" {
            return Err("cancelled".into());
        }
        Ok(result.final_text)
    }
}

fn run_tool(
    tool: Option<Arc<dyn Tool>>,
    call: &ToolCallPart,
    ctx: &ToolContext,
) -> ToolResultPart {
    use crate::hooks::{run_hook, HookEvent};

    // Pre hooks: any failing hook blocks the call; its output is the error
    // result the model sees.
    for hook in ctx.hooks.iter().filter(|h| h.matches(HookEvent::Pre, &call.name)) {
        let outcome = run_hook(hook, &call.name, &call.input, None, &ctx.cwd);
        if !outcome.success {
            return ToolResultPart {
                tool_call_id: call.id.clone(),
                tool_name: call.name.clone(),
                content: format!(
                    "[hook \"{}\"] blocked this call:\n{}",
                    hook.name,
                    if outcome.output.is_empty() { "(no output)" } else { &outcome.output }
                ),
                is_error: true,
            };
        }
    }

    let output = match tool {
        Some(t) => t.execute(&call.input, ctx),
        None => ToolOutput::err(format!("unknown tool: {}", call.name)),
    };
    let mut result = ToolResultPart {
        tool_call_id: call.id.clone(),
        tool_name: call.name.clone(),
        content: output.content,
        is_error: output.is_error,
    };

    // Post hooks: a failing hook appends feedback (lint-on-edit) — the
    // tool's own result stands, the model sees the problems immediately.
    for hook in ctx.hooks.iter().filter(|h| h.matches(HookEvent::Post, &call.name)) {
        let outcome = run_hook(hook, &call.name, &call.input, Some(&result.content), &ctx.cwd);
        if !outcome.success && !outcome.output.is_empty() {
            result.content.push_str(&format!(
                "\n\n[hook \"{}\" reported problems — address them]:\n{}",
                hook.name, outcome.output
            ));
        }
    }
    result
}

fn preview(s: &str) -> String {
    let first = s.lines().next().unwrap_or("");
    let shown: String = first.chars().take(160).collect();
    shown
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::builtin_tools;

    #[test]
    fn nested_toolset_is_read_only_and_recursion_free() {
        let names: Vec<String> = nested_toolset(&builtin_tools())
            .iter()
            .map(|t| t.def().name.clone())
            .collect();
        for banned in ["subagent", "todo_write", "remember", "write", "edit", "bash", "ask_user"] {
            assert!(!names.contains(&banned.to_string()), "{banned} must be excluded");
        }
        for expected in ["read", "grep", "glob", "recall"] {
            assert!(names.contains(&expected.to_string()), "{expected} must be included");
        }
    }
}

// keep Value import used even if future refactors drop direct uses
#[allow(unused)]
fn _t(_: &Value) {}
