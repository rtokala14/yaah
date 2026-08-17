//! The agent loop: one flat loop, no planner/executor split.
//!
//! Per turn: stream the model → if it called tools, execute them (adjacent
//! read-only calls in parallel) → append results → manage context → repeat.
//! Evidence-grounded completion: if the model claims done after mutating
//! files without running anything, inject a one-time system-note nudge and
//! continue (deterministic harness gate; no prompt scaffolding).

use crate::context::{self, ContextOptions};
use crate::types::*;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

const MUTATING_TOOLS: &[&str] = &["write", "edit"];
const VERIFYING_TOOLS: &[&str] = &["bash"];

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
        let tool_ctx = Arc::new(ToolContext {
            cwd: opts.cwd.clone(),
            cancel,
            read_files: std::sync::Mutex::new(HashMap::new()),
        });
        Self {
            opts,
            messages: Vec::new(),
            tools_by_name,
            tool_ctx,
            unverified_mutation: false,
            verify_nudge_used: false,
        }
    }

    pub fn messages(&self) -> &[AgentMessage] {
        &self.messages
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
            emit(AgentEvent::TurnStart { turn });

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
        results.into_iter().flatten().collect()
    }

    fn manage_context(&mut self, emit: &mut dyn FnMut(AgentEvent), cancel: &CancelToken) {
        let opts = &self.opts.context;
        let mut tokens = context::estimate_tokens(&self.messages);

        if tokens as f32 > opts.budget_tokens as f32 * opts.prune_at {
            let pruned = context::prune_tool_results(&mut self.messages, opts);
            if pruned > 0 {
                emit(AgentEvent::Pruned { count: pruned });
                tokens = context::estimate_tokens(&self.messages);
            }
        }

        if tokens as f32 > opts.budget_tokens as f32 * opts.compact_at {
            emit(AgentEvent::Compaction { before_tokens: tokens });
            match context::compact(
                self.opts.provider.as_ref(),
                &self.opts.system,
                &self.messages,
                cancel,
            ) {
                Ok(replacement) => self.messages = replacement,
                Err(e) => emit(AgentEvent::Error(format!("compaction failed: {e}"))),
            }
        }
    }
}

fn run_tool(
    tool: Option<Arc<dyn Tool>>,
    call: &ToolCallPart,
    ctx: &ToolContext,
) -> ToolResultPart {
    let output = match tool {
        Some(t) => t.execute(&call.input, ctx),
        None => ToolOutput::err(format!("unknown tool: {}", call.name)),
    };
    ToolResultPart {
        tool_call_id: call.id.clone(),
        tool_name: call.name.clone(),
        content: output.content,
        is_error: output.is_error,
    }
}

fn preview(s: &str) -> String {
    let first = s.lines().next().unwrap_or("");
    let shown: String = first.chars().take(160).collect();
    shown
}

// keep Value import used even if future refactors drop direct uses
#[allow(unused)]
fn _t(_: &Value) {}
