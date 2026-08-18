//! Context management for arbitrarily long sessions.
//!
//! Escalation ladder — cheapest first, each step only if still over budget:
//! 1. **Prune** stale tool results: old, large results are replaced in place
//!    with a short stub. The filesystem is the real memory — re-running a
//!    read is cheap. Batched at a threshold rather than per-turn, to
//!    amortize the prompt-cache invalidation it causes.
//! 2. **Strip stale thinking**: reasoning blocks from old turns are dropped
//!    (providers ignore them on replay; only recent reasoning matters).
//! 3. **Force-prune**: every prunable tool result outside the current turn,
//!    regardless of age. Still lossless in the "re-readable" sense.
//! 4. **Compact** (last resort): the model writes a structured handoff and
//!    the transcript restarts as [original task, verbatim] + [summary] +
//!    [recent tail kept verbatim]. Pinning the task keeps multi-compaction
//!    sessions from drifting; the verbatim tail keeps in-flight work sharp.

use crate::types::*;
use serde::{Deserialize, Serialize};

/// Durable session memory: survives compaction (re-injected verbatim into
/// every compacted transcript) and app restarts (persisted in the session
/// journal).
///
/// - `notes`: agent-authored via the `remember` tool — decisions,
///   constraints, learned facts. The curated, high-value channel.
/// - `summaries`: every compaction's handoff summary, archived in order.
///   The historical record; not re-injected (the newest summary is already
///   in the transcript), but inspectable and available to future features.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionMemory {
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    pub summaries: Vec<String>,
}

/// Character budget for the injected digest — memory must never become the
/// context problem it exists to solve.
const MEMORY_DIGEST_MAX_CHARS: usize = 12_000;

/// Render notes as the digest message injected on compaction. When over
/// budget, the oldest and newest notes win (foundational decisions and
/// fresh learnings); the middle is elided with a marker.
pub fn memory_digest(memory: &SessionMemory) -> Option<String> {
    if memory.notes.is_empty() {
        return None;
    }
    let total: usize = memory.notes.iter().map(|n| n.len() + 3).sum();
    let selected: Vec<String> = if total <= MEMORY_DIGEST_MAX_CHARS {
        memory.notes.iter().map(|n| format!("- {n}")).collect()
    } else {
        let mut front = Vec::new();
        let mut used = 0usize;
        let mut i = 0usize;
        while i < memory.notes.len() && used + memory.notes[i].len() < MEMORY_DIGEST_MAX_CHARS / 3
        {
            used += memory.notes[i].len() + 3;
            front.push(format!("- {}", memory.notes[i]));
            i += 1;
        }
        let mut back = Vec::new();
        let mut j = memory.notes.len();
        while j > i && used + memory.notes[j - 1].len() < MEMORY_DIGEST_MAX_CHARS {
            j -= 1;
            used += memory.notes[j].len() + 3;
            back.push(format!("- {}", memory.notes[j]));
        }
        back.reverse();
        let mut out = front;
        if j > i {
            out.push(format!("- [{} older notes elided]", j - i));
        }
        out.extend(back);
        out
    };
    Some(format!(
        "<session-memory>\nDurable notes recorded during this session (via the remember tool):\n{}\n</session-memory>",
        selected.join("\n")
    ))
}

/// Cheap token estimate: ~4 chars/token. Used only for budgeting decisions;
/// real usage from the provider recalibrates nothing here by design (the
/// estimate only has to be monotone, not accurate).
pub fn estimate_tokens(messages: &[AgentMessage]) -> usize {
    let mut chars = 0usize;
    for m in messages {
        match m {
            AgentMessage::SystemNote { content } => chars += content.len(),
            AgentMessage::User { content } => {
                for p in content {
                    chars += match p {
                        UserPart::Text { text } => text.len(),
                        UserPart::ToolResult(r) => r.content.len(),
                        UserPart::Image { .. } => 6000,
                    };
                }
            }
            AgentMessage::Assistant { content } => {
                for p in content {
                    chars += match p {
                        AssistantPart::Text { text } | AssistantPart::Thinking { text, .. } => {
                            text.len()
                        }
                        AssistantPart::ToolCall(c) => c.input.to_string().len() + 50,
                    };
                }
            }
        }
    }
    chars / 4 + 1
}

#[derive(Debug, Clone)]
pub struct ContextOptions {
    /// Hard budget for the conversation, in estimated tokens.
    pub budget_tokens: usize,
    /// Start pruning at this fraction of budget.
    pub prune_at: f32,
    /// Compact at this fraction of budget.
    pub compact_at: f32,
    /// Tool results within the last N assistant turns are never pruned.
    pub keep_recent_turns: usize,
    /// Tool results smaller than this are never pruned.
    pub min_prune_chars: usize,
    /// Messages kept verbatim through compaction (boundary-aligned).
    pub compact_keep_tail: usize,
}

impl Default for ContextOptions {
    fn default() -> Self {
        Self {
            budget_tokens: 160_000,
            prune_at: 0.5,
            compact_at: 0.8,
            keep_recent_turns: 3,
            min_prune_chars: 2000,
            compact_keep_tail: 12,
        }
    }
}

impl ContextOptions {
    /// Budget derived from a model's context window: 80% of the window,
    /// leaving headroom for the system prompt, tools, and the reply.
    pub fn for_context_window(window: Option<u32>) -> Self {
        let mut opts = Self::default();
        if let Some(w) = window {
            opts.budget_tokens = (w as usize * 4 / 5).max(16_000);
        }
        opts
    }
}

const PRUNE_MARKER: &str = "[stale ";

fn prune_stub(tool: &str, chars: usize) -> String {
    format!("[stale {tool} result pruned ({chars} chars). Re-run the tool if you need it again.]")
}

/// Index of the message that starts the Nth-from-last assistant turn;
/// everything before it counts as "old".
fn recent_cutoff(messages: &[AgentMessage], keep_recent_turns: usize) -> usize {
    let mut assistant_seen = 0usize;
    for i in (0..messages.len()).rev() {
        if matches!(messages[i], AgentMessage::Assistant { .. }) {
            assistant_seen += 1;
            if assistant_seen >= keep_recent_turns {
                return i;
            }
        }
    }
    0
}

/// Replace old large tool results with restorable stubs, in place.
/// Returns how many results were pruned.
pub fn prune_tool_results(messages: &mut [AgentMessage], opts: &ContextOptions) -> usize {
    prune_with_keep(messages, opts, opts.keep_recent_turns)
}

/// Escalation step: prune every prunable result outside the current turn.
pub fn force_prune_tool_results(messages: &mut [AgentMessage], opts: &ContextOptions) -> usize {
    prune_with_keep(messages, opts, 1)
}

fn prune_with_keep(
    messages: &mut [AgentMessage],
    opts: &ContextOptions,
    keep_recent_turns: usize,
) -> usize {
    let cutoff = recent_cutoff(messages, keep_recent_turns);
    let mut pruned = 0usize;
    for m in messages.iter_mut().take(cutoff) {
        if let AgentMessage::User { content } = m {
            for p in content.iter_mut() {
                if let UserPart::ToolResult(r) = p {
                    if r.content.len() >= opts.min_prune_chars
                        && !r.content.starts_with(PRUNE_MARKER)
                    {
                        r.content = prune_stub(&r.tool_name, r.content.len());
                        pruned += 1;
                    }
                }
            }
        }
    }
    pruned
}

/// Drop reasoning blocks from turns older than the recency window.
/// Providers ignore prior-turn thinking on replay, so this is lossless for
/// the model while often reclaiming a large share of a long transcript.
/// Returns how many blocks were removed.
pub fn strip_stale_thinking(messages: &mut [AgentMessage], keep_recent_turns: usize) -> usize {
    let cutoff = recent_cutoff(messages, keep_recent_turns);
    let mut stripped = 0usize;
    for m in messages.iter_mut().take(cutoff) {
        if let AgentMessage::Assistant { content } = m {
            let before = content.len();
            content.retain(|p| !matches!(p, AssistantPart::Thinking { .. }));
            stripped += before - content.len();
            if content.is_empty() {
                // An assistant message must keep some content.
                content.push(AssistantPart::Text { text: "[reasoning elided]".into() });
            }
        }
    }
    stripped
}

pub const COMPACT_PROMPT: &str = "Write a handoff summary of this session for another engineer (an AI agent) who will continue the work with NO other context. Include, in this order:\n1. Goal: what the user asked for, verbatim where it matters.\n2. State: what has been done and verified so far (files created/modified with paths, commands run, test results).\n3. Key knowledge: APIs, file locations, conventions, and constraints discovered — anything expensive to rediscover.\n4. In progress / next steps: what remains, in priority order, with enough detail to resume mid-step.\n5. Pitfalls: dead ends already explored and errors already fixed, so they are not repeated.\nBe dense and factual. Do not editorialize. Use paths and identifiers exactly.";

/// First safe index at or after `from` where the transcript can be cut:
/// a tail must not begin with tool results whose calls were summarized away.
fn safe_tail_start(messages: &[AgentMessage], from: usize) -> usize {
    for i in from..messages.len() {
        match &messages[i] {
            AgentMessage::Assistant { .. } | AgentMessage::SystemNote { .. } => return i,
            AgentMessage::User { content } => {
                if !content.iter().any(|p| matches!(p, UserPart::ToolResult(_))) {
                    return i;
                }
            }
        }
    }
    messages.len()
}

/// The user's original task, pinned verbatim through every compaction so a
/// many-times-compacted session cannot drift from what was asked. An
/// already-pinned task (from a previous compaction) is unwrapped, never
/// re-wrapped recursively.
fn original_task(messages: &[AgentMessage]) -> Option<String> {
    let texts = messages.iter().filter_map(|m| match m {
        AgentMessage::User { content } => content.iter().find_map(|p| match p {
            UserPart::Text { text } if !text.trim().is_empty() => Some(text.as_str()),
            _ => None,
        }),
        _ => None,
    });
    let mut first_plain: Option<String> = None;
    for text in texts {
        if let Some(inner) = unwrap_pinned_task(text) {
            return Some(inner);
        }
        if first_plain.is_none()
            && !text.starts_with("<session-summary>")
            && !text.starts_with("<session-memory>")
        {
            first_plain = Some(text.to_string());
        }
    }
    first_plain
}

fn unwrap_pinned_task(text: &str) -> Option<String> {
    let body = text.strip_prefix("<original-task>")?;
    let body = body.strip_suffix("</original-task>").unwrap_or(body);
    let body = body.trim();
    let body = body.strip_prefix("The user's original request, verbatim:").unwrap_or(body);
    Some(body.trim().to_string())
}

/// Compact the transcript via a model-written summary. Returns the
/// replacement message list —
/// [pinned original task] + [memory digest] + [summary] + [tail, verbatim]
/// — plus the summary text so the caller can archive it in session memory.
pub fn compact(
    provider: &dyn Provider,
    system: &str,
    messages: &[AgentMessage],
    memory: &SessionMemory,
    opts: &ContextOptions,
    cancel: &CancelToken,
) -> Result<(Vec<AgentMessage>, String), ProviderError> {
    let tail_from = messages.len().saturating_sub(opts.compact_keep_tail);
    let tail_start = safe_tail_start(messages, tail_from);
    let head = &messages[..tail_start];
    if head.is_empty() {
        return Ok((messages.to_vec(), String::new())); // nothing to summarize
    }

    // Guard the summarization request itself: if the head alone exceeds the
    // budget, keep its first and last portions and omit the middle (the
    // ends carry the goal and the current state; the middle is the most
    // summarizable part anyway).
    let mut req_messages: Vec<AgentMessage>;
    if estimate_tokens(head) > opts.budget_tokens {
        let keep_front = 2.min(head.len());
        let mut back_start = head.len();
        let mut back_tokens = 0usize;
        while back_start > keep_front {
            let candidate = back_start - 1;
            back_tokens += estimate_tokens(&head[candidate..candidate + 1]);
            if back_tokens > opts.budget_tokens / 2 {
                break;
            }
            back_start = candidate;
        }
        let back_start = safe_tail_start(head, back_start);
        req_messages = head[..keep_front].to_vec();
        req_messages.push(AgentMessage::SystemNote {
            content: format!(
                "[{} intermediate messages omitted from this summarization input]",
                back_start.saturating_sub(keep_front)
            ),
        });
        req_messages.extend_from_slice(&head[back_start..]);
    } else {
        req_messages = head.to_vec();
    }
    req_messages.push(AgentMessage::User {
        content: vec![UserPart::Text { text: COMPACT_PROMPT.to_string() }],
    });

    let req = ProviderRequest {
        system: system.to_string(),
        messages: req_messages,
        tools: vec![],
        max_tokens: 6000,
        effort: Some(Effort::Low),
        temperature: None,
    };
    let turn = provider.stream(&req, &mut |_| {}, cancel)?;
    let summary: String = turn
        .content
        .iter()
        .filter_map(|p| match p {
            AssistantPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    let mut replacement = Vec::new();
    if let Some(task) = original_task(messages) {
        replacement.push(AgentMessage::User {
            content: vec![UserPart::Text {
                text: format!(
                    "<original-task>\nThe user's original request, verbatim:\n\n{task}\n</original-task>"
                ),
            }],
        });
    }
    if let Some(digest) = memory_digest(memory) {
        replacement.push(AgentMessage::User { content: vec![UserPart::Text { text: digest }] });
    }
    replacement.push(AgentMessage::User {
        content: vec![UserPart::Text {
            text: format!(
                "<session-summary>\nThe conversation so far was compacted. Summary:\n\n{summary}\n</session-summary>\nContinue the task from where the summary leaves off."
            ),
        }],
    });
    replacement.extend_from_slice(&messages[tail_start..]);
    Ok((replacement, summary))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn user(text: &str) -> AgentMessage {
        AgentMessage::User { content: vec![UserPart::Text { text: text.into() }] }
    }

    fn assistant_call(tool: &str, id: &str) -> AgentMessage {
        AgentMessage::Assistant {
            content: vec![
                AssistantPart::Thinking {
                    text: "reasoning ".repeat(50),
                    signature: None,
                    raw: None,
                },
                AssistantPart::ToolCall(ToolCallPart {
                    id: id.into(),
                    name: tool.into(),
                    input: serde_json::json!({}),
                    raw_arguments: None,
                }),
            ],
        }
    }

    fn tool_result(tool: &str, id: &str, size: usize) -> AgentMessage {
        AgentMessage::User {
            content: vec![UserPart::ToolResult(ToolResultPart {
                tool_call_id: id.into(),
                tool_name: tool.into(),
                content: "x".repeat(size),
                is_error: false,
            })],
        }
    }

    fn long_transcript(pairs: usize) -> Vec<AgentMessage> {
        let mut m = vec![user("build the feature")];
        for i in 0..pairs {
            m.push(assistant_call("read", &format!("t{i}")));
            m.push(tool_result("read", &format!("t{i}"), 5000));
        }
        m.push(AgentMessage::Assistant {
            content: vec![AssistantPart::Text { text: "done step".into() }],
        });
        m
    }

    #[test]
    fn force_prune_reaches_older_results_normal_prune_spares() {
        let opts = ContextOptions::default();
        // Realistic pre-request shape: the transcript ends with the current
        // turn's tool results (manage_context runs before the next request).
        let mut a = long_transcript(6);
        a.pop();
        let mut b = a.clone();
        let normal = prune_tool_results(&mut a, &opts);
        let forced = force_prune_tool_results(&mut b, &opts);
        assert!(forced > normal, "force ({forced}) must prune more than normal ({normal})");
        // The in-flight turn's results are always spared.
        assert!(matches!(
            b.last().unwrap(),
            AgentMessage::User { content }
                if matches!(&content[0], UserPart::ToolResult(r) if !r.content.starts_with("[stale"))
        ));
    }

    #[test]
    fn strip_stale_thinking_keeps_recent_and_never_empties_messages() {
        let mut m = long_transcript(5);
        // One assistant message that is thinking-only.
        m.insert(
            1,
            AgentMessage::Assistant {
                content: vec![AssistantPart::Thinking {
                    text: "only thoughts".into(),
                    signature: None,
                    raw: None,
                }],
            },
        );
        let stripped = strip_stale_thinking(&mut m, 2);
        assert!(stripped > 0);
        // No assistant message is ever left empty.
        for msg in &m {
            if let AgentMessage::Assistant { content } = msg {
                assert!(!content.is_empty());
            }
        }
        // Recent turns keep their thinking.
        let recent_thinking = m.iter().rev().take(3).any(|msg| {
            matches!(msg, AgentMessage::Assistant { content }
                if content.iter().any(|p| matches!(p, AssistantPart::Thinking { .. })))
        });
        assert!(recent_thinking);
    }

    #[test]
    fn safe_tail_never_starts_with_orphan_tool_results() {
        let m = long_transcript(4);
        for from in 0..m.len() {
            let start = safe_tail_start(&m, from);
            if start < m.len() {
                if let AgentMessage::User { content } = &m[start] {
                    assert!(!content
                        .iter()
                        .any(|p| matches!(p, UserPart::ToolResult(_))));
                }
            }
        }
    }

    struct FakeProvider {
        pub last_request_tokens: Mutex<usize>,
    }
    impl Provider for FakeProvider {
        fn name(&self) -> &str {
            "fake"
        }
        fn model(&self) -> &str {
            "fake-1"
        }
        fn stream(
            &self,
            req: &ProviderRequest,
            _on_delta: &mut dyn FnMut(StreamDelta),
            _cancel: &CancelToken,
        ) -> Result<AssistantTurn, ProviderError> {
            *self.last_request_tokens.lock().unwrap() = estimate_tokens(&req.messages);
            Ok(AssistantTurn {
                content: vec![AssistantPart::Text { text: "SUMMARY".into() }],
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
            })
        }
    }

    #[test]
    fn compact_pins_task_keeps_tail_and_caps_request() {
        let opts = ContextOptions {
            budget_tokens: 10_000, // far below the transcript size
            compact_keep_tail: 4,
            ..Default::default()
        };
        let messages = long_transcript(40); // ~50k estimated tokens
        let provider = FakeProvider { last_request_tokens: Mutex::new(0) };
        let (out, summary) = compact(
            &provider,
            "sys",
            &messages,
            &SessionMemory::default(),
            &opts,
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(summary, "SUMMARY");

        // Original task pinned verbatim, summary present, tail kept.
        assert!(matches!(&out[0], AgentMessage::User { content }
            if matches!(&content[0], UserPart::Text { text } if text.contains("build the feature"))));
        assert!(matches!(&out[1], AgentMessage::User { content }
            if matches!(&content[0], UserPart::Text { text } if text.contains("SUMMARY"))));
        assert!(out.len() > 2, "verbatim tail must survive");
        // Tail is boundary-safe.
        if let AgentMessage::User { content } = &out[2] {
            assert!(!content.iter().any(|p| matches!(p, UserPart::ToolResult(_))));
        }
        // The summarization request itself was capped near the budget.
        let sent = *provider.last_request_tokens.lock().unwrap();
        assert!(
            sent < opts.budget_tokens + 2000,
            "summarization request ({sent} tok) must respect the budget"
        );
        // And the result is much smaller than the input.
        assert!(estimate_tokens(&out) < estimate_tokens(&messages) / 4);
    }

    #[test]
    fn compact_after_compact_keeps_original_task_pinned() {
        let opts = ContextOptions { compact_keep_tail: 2, ..Default::default() };
        let provider = FakeProvider { last_request_tokens: Mutex::new(0) };
        let memory = SessionMemory {
            notes: vec!["ci needs libxkbcommon-dev".into()],
            summaries: vec![],
        };
        let messages = long_transcript(10);
        let (once, _) =
            compact(&provider, "sys", &messages, &memory, &opts, &CancelToken::new()).unwrap();
        let (twice, _) =
            compact(&provider, "sys", &once, &memory, &opts, &CancelToken::new()).unwrap();
        // The pinned task survives a second compaction and is not the
        // synthetic wrapper being re-pinned recursively.
        let pinned = match &twice[0] {
            AgentMessage::User { content } => match &content[0] {
                UserPart::Text { text } => text.clone(),
                _ => String::new(),
            },
            _ => String::new(),
        };
        assert!(pinned.contains("build the feature"));
        assert!(!pinned.contains("<original-task>\nThe user's original request, verbatim:\n\n<original-task>"));
        // Memory notes are re-injected verbatim on every compaction.
        for out in [&once, &twice] {
            let has_digest = out.iter().any(|m| matches!(m, AgentMessage::User { content }
                if matches!(&content[0], UserPart::Text { text }
                    if text.starts_with("<session-memory>") && text.contains("libxkbcommon-dev"))));
            assert!(has_digest, "memory digest must appear after compaction");
        }
    }

    #[test]
    fn memory_digest_caps_size_keeping_oldest_and_newest() {
        let mut memory = SessionMemory::default();
        for i in 0..400 {
            memory.notes.push(format!("note number {i} with some padding text to add bulk"));
        }
        let digest = memory_digest(&memory).unwrap();
        assert!(digest.len() < 14_000, "digest must stay bounded, got {}", digest.len());
        assert!(digest.contains("note number 0"), "oldest notes survive");
        assert!(digest.contains("note number 399"), "newest notes survive");
        assert!(digest.contains("older notes elided"));
        // Small memories are rendered whole, no markers.
        let small = SessionMemory { notes: vec!["a".into(), "b".into()], summaries: vec![] };
        let d = memory_digest(&small).unwrap();
        assert!(d.contains("- a") && d.contains("- b") && !d.contains("elided"));
        assert!(memory_digest(&SessionMemory::default()).is_none());
    }

    #[test]
    fn budget_follows_model_context_window() {
        assert_eq!(ContextOptions::for_context_window(Some(1_000_000)).budget_tokens, 800_000);
        assert_eq!(ContextOptions::for_context_window(None).budget_tokens, 160_000);
        // Tiny windows keep a workable floor.
        assert_eq!(ContextOptions::for_context_window(Some(8_000)).budget_tokens, 16_000);
    }
}
