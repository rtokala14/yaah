//! Context management: prune first (cheap, restorable), compact last.
//!
//! 1. Tool-result aging ("prune"): old, large tool results are replaced in
//!    place with a short stub. The filesystem is the real memory — re-running
//!    a read is cheap. Pruning is batched at a threshold rather than done
//!    per-turn, to amortize the prompt-cache invalidation it causes.
//! 2. Compaction ("summarize"): the model writes a structured handoff
//!    summary and the transcript restarts as [summary]. Full cache rebuild —
//!    last resort only.

use crate::types::*;

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
}

impl Default for ContextOptions {
    fn default() -> Self {
        Self {
            budget_tokens: 160_000,
            prune_at: 0.5,
            compact_at: 0.8,
            keep_recent_turns: 3,
            min_prune_chars: 2000,
        }
    }
}

const PRUNE_MARKER: &str = "[stale ";

fn prune_stub(tool: &str, chars: usize) -> String {
    format!("[stale {tool} result pruned ({chars} chars). Re-run the tool if you need it again.]")
}

/// Replace old large tool results with restorable stubs, in place.
/// Returns how many results were pruned.
pub fn prune_tool_results(messages: &mut [AgentMessage], opts: &ContextOptions) -> usize {
    // Find the cutoff index: everything before the Nth-from-last assistant
    // turn is prunable.
    let mut assistant_seen = 0usize;
    let mut cutoff = 0usize;
    for i in (0..messages.len()).rev() {
        if matches!(messages[i], AgentMessage::Assistant { .. }) {
            assistant_seen += 1;
            if assistant_seen >= opts.keep_recent_turns {
                cutoff = i;
                break;
            }
        }
    }

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

pub const COMPACT_PROMPT: &str = "Write a handoff summary of this session for another engineer (an AI agent) who will continue the work with NO other context. Include, in this order:\n1. Goal: what the user asked for, verbatim where it matters.\n2. State: what has been done and verified so far (files created/modified with paths, commands run, test results).\n3. Key knowledge: APIs, file locations, conventions, and constraints discovered — anything expensive to rediscover.\n4. In progress / next steps: what remains, in priority order, with enough detail to resume mid-step.\n5. Pitfalls: dead ends already explored and errors already fixed, so they are not repeated.\nBe dense and factual. Do not editorialize. Use paths and identifiers exactly.";

/// Compact the transcript via a model-written summary. Returns the
/// replacement message list.
pub fn compact(
    provider: &dyn Provider,
    system: &str,
    messages: &[AgentMessage],
    cancel: &CancelToken,
) -> Result<Vec<AgentMessage>, ProviderError> {
    let mut req_messages = messages.to_vec();
    req_messages.push(AgentMessage::User {
        content: vec![UserPart::Text { text: COMPACT_PROMPT.to_string() }],
    });
    let req = ProviderRequest {
        system: system.to_string(),
        messages: req_messages,
        tools: vec![],
        max_tokens: 4000,
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

    Ok(vec![AgentMessage::User {
        content: vec![UserPart::Text {
            text: format!(
                "<session-summary>\nThe conversation so far was compacted. Summary:\n\n{summary}\n</session-summary>\nContinue the task from where the summary leaves off."
            ),
        }],
    }])
}
