//! Render-ready transcript state, built by folding `SessionEvent`s — or, on
//! session restore, by folding the persisted `AgentMessage` history.
//! GPUI-free so it can be unit-tested; the chat view just draws it.

use harness_core::types::{
    AgentEvent, AgentMessage, AssistantPart, StopReason, Usage, UserPart,
};
use harness_core::SessionEvent;

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    UserMessage { text: String },
    AssistantText { text: String, streaming: bool },
    Thinking { text: String, streaming: bool },
    ToolCall {
        name: String,
        input_summary: String,
        output_preview: String,
        is_error: bool,
        duration_ms: u64,
        done: bool,
    },
    Notice { text: String },
    Error { text: String },
}

#[derive(Debug, Default)]
pub struct Transcript {
    pub blocks: Vec<Block>,
    pub running: bool,
    pub usage: Usage,
    pub turns: u32,
    pub last_stop: Option<String>,
}

impl Transcript {
    /// Rebuild a transcript from a persisted message history (session
    /// restore). Everything renders as finished; usage/turns come from the
    /// journal.
    pub fn from_messages(messages: &[AgentMessage], usage: Usage, turns: u32) -> Self {
        let mut t = Transcript::default();
        for m in messages {
            match m {
                AgentMessage::User { content } => {
                    for part in content {
                        match part {
                            UserPart::Text { text } => {
                                t.blocks.push(Block::UserMessage { text: text.clone() });
                            }
                            UserPart::ToolResult(r) => {
                                // Attach to the earliest unfinished call with
                                // this tool name (results follow their calls).
                                if let Some(Block::ToolCall {
                                    output_preview,
                                    is_error,
                                    done,
                                    ..
                                }) = t.blocks.iter_mut().find(|b| {
                                    matches!(b, Block::ToolCall { name, done: false, .. }
                                        if *name == r.tool_name)
                                }) {
                                    *output_preview = preview(&r.content);
                                    *is_error = r.is_error;
                                    *done = true;
                                }
                            }
                            UserPart::Image { .. } => {}
                        }
                    }
                }
                AgentMessage::Assistant { content } => {
                    for part in content {
                        match part {
                            AssistantPart::Text { text } if !text.is_empty() => {
                                t.blocks.push(Block::AssistantText {
                                    text: text.clone(),
                                    streaming: false,
                                });
                            }
                            AssistantPart::Thinking { text, .. } if !text.is_empty() => {
                                t.blocks
                                    .push(Block::Thinking { text: text.clone(), streaming: false });
                            }
                            AssistantPart::ToolCall(c) => {
                                let summary =
                                    serde_json::to_string(&c.input).unwrap_or_default();
                                t.blocks.push(Block::ToolCall {
                                    name: c.name.clone(),
                                    input_summary: summary.chars().take(140).collect(),
                                    output_preview: String::new(),
                                    is_error: false,
                                    duration_ms: 0,
                                    done: false,
                                });
                            }
                            _ => {}
                        }
                    }
                }
                AgentMessage::SystemNote { content } => {
                    t.blocks.push(Block::Notice { text: preview(content) });
                }
            }
        }
        t.usage = usage;
        t.turns = turns;
        t
    }

    pub fn push_user(&mut self, text: &str) {
        self.blocks.push(Block::UserMessage { text: text.to_string() });
        self.running = true;
        self.last_stop = None;
    }

    pub fn apply(&mut self, event: &SessionEvent) {
        match event {
            SessionEvent::Agent(ev) => self.apply_agent(ev),
            SessionEvent::RunFinished { turns, usage, stop_reason, .. } => {
                self.running = false;
                self.turns = *turns;
                self.usage = *usage;
                self.last_stop = Some(stop_reason.clone());
                self.finish_streaming();
            }
            SessionEvent::ProfileChanged { label } => {
                self.blocks.push(Block::Notice {
                    text: format!("switched to {label} (prompt-cache prefix resets)"),
                });
            }
            SessionEvent::Fatal(msg) => {
                self.running = false;
                self.blocks.push(Block::Error { text: msg.clone() });
            }
        }
    }

    fn apply_agent(&mut self, ev: &AgentEvent) {
        match ev {
            AgentEvent::TurnStart { .. } => self.finish_streaming(),
            AgentEvent::TextDelta(d) => {
                if let Some(Block::AssistantText { text, streaming: true }) =
                    self.blocks.last_mut()
                {
                    text.push_str(d);
                } else {
                    self.finish_streaming();
                    self.blocks
                        .push(Block::AssistantText { text: d.clone(), streaming: true });
                }
            }
            AgentEvent::ThinkingDelta(d) => {
                if let Some(Block::Thinking { text, streaming: true }) = self.blocks.last_mut() {
                    text.push_str(d);
                } else {
                    self.finish_streaming();
                    self.blocks.push(Block::Thinking { text: d.clone(), streaming: true });
                }
            }
            AgentEvent::ToolStart { name, input } => {
                self.finish_streaming();
                let summary = serde_json::to_string(input).unwrap_or_default();
                let summary: String = summary.chars().take(140).collect();
                self.blocks.push(Block::ToolCall {
                    name: name.clone(),
                    input_summary: summary,
                    output_preview: String::new(),
                    is_error: false,
                    duration_ms: 0,
                    done: false,
                });
            }
            AgentEvent::ToolEnd { name, output_preview, is_error, duration_ms } => {
                // Match the most recent unfinished call with this name (parallel
                // read-only batches can interleave).
                if let Some(Block::ToolCall {
                    output_preview: out,
                    is_error: err,
                    duration_ms: ms,
                    done,
                    ..
                }) = self.blocks.iter_mut().rev().find(|b| {
                    matches!(b, Block::ToolCall { name: n, done: false, .. } if n == name)
                }) {
                    *out = output_preview.clone();
                    *err = *is_error;
                    *ms = *duration_ms;
                    *done = true;
                }
            }
            AgentEvent::Pruned { count } => {
                self.blocks.push(Block::Notice {
                    text: format!("pruned {count} stale tool results from context"),
                });
            }
            AgentEvent::Compaction { before_tokens } => {
                self.blocks.push(Block::Notice {
                    text: format!("compacting context (~{before_tokens} tokens)"),
                });
            }
            AgentEvent::TurnEnd { stop_reason, .. } => {
                if *stop_reason != StopReason::ToolUse {
                    self.finish_streaming();
                }
            }
            AgentEvent::Error(msg) => {
                self.blocks.push(Block::Error { text: msg.clone() });
            }
            AgentEvent::Done { .. } => self.finish_streaming(),
        }
    }

    fn finish_streaming(&mut self) {
        for b in self.blocks.iter_mut().rev() {
            match b {
                Block::AssistantText { streaming, .. } | Block::Thinking { streaming, .. } => {
                    if *streaming {
                        *streaming = false;
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }
    }
}

/// First line, capped — same shape the live ToolEnd previews use.
fn preview(s: &str) -> String {
    s.lines().next().unwrap_or("").chars().take(160).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_core::types::AgentEvent as E;

    #[test]
    fn folds_stream_into_blocks() {
        let mut t = Transcript::default();
        t.push_user("do the thing");
        t.apply(&SessionEvent::Agent(E::TextDelta("Working".into())));
        t.apply(&SessionEvent::Agent(E::TextDelta(" on it".into())));
        t.apply(&SessionEvent::Agent(E::ToolStart {
            name: "read".into(),
            input: serde_json::json!({"path": "a.rs"}),
        }));
        t.apply(&SessionEvent::Agent(E::ToolEnd {
            name: "read".into(),
            output_preview: "    1\tfn main".into(),
            is_error: false,
            duration_ms: 3,
        }));
        t.apply(&SessionEvent::RunFinished {
            final_text: "done".into(),
            turns: 1,
            usage: Default::default(),
            stop_reason: "end_turn".into(),
        });

        assert_eq!(t.blocks.len(), 3);
        assert!(matches!(&t.blocks[1], Block::AssistantText { text, streaming: false } if text == "Working on it"));
        assert!(matches!(&t.blocks[2], Block::ToolCall { done: true, .. }));
        assert!(!t.running);
    }

    #[test]
    fn restores_from_persisted_messages() {
        use harness_core::types::{ToolCallPart, ToolResultPart};
        let messages = vec![
            AgentMessage::User {
                content: vec![UserPart::Text { text: "fix the bug".into() }],
            },
            AgentMessage::Assistant {
                content: vec![
                    AssistantPart::Thinking {
                        text: "looking".into(),
                        signature: None,
                        raw: None,
                    },
                    AssistantPart::ToolCall(ToolCallPart {
                        id: "t1".into(),
                        name: "read".into(),
                        input: serde_json::json!({"path": "a.rs"}),
                        raw_arguments: None,
                    }),
                ],
            },
            AgentMessage::User {
                content: vec![UserPart::ToolResult(ToolResultPart {
                    tool_call_id: "t1".into(),
                    tool_name: "read".into(),
                    content: "line one\nline two".into(),
                    is_error: false,
                })],
            },
            AgentMessage::Assistant {
                content: vec![AssistantPart::Text { text: "fixed".into() }],
            },
            AgentMessage::SystemNote { content: "verify first".into() },
        ];
        let usage = Usage { input_tokens: 100, output_tokens: 40, ..Default::default() };
        let t = Transcript::from_messages(&messages, usage, 2);

        assert!(matches!(&t.blocks[0], Block::UserMessage { text } if text == "fix the bug"));
        assert!(matches!(&t.blocks[1], Block::Thinking { streaming: false, .. }));
        assert!(matches!(
            &t.blocks[2],
            Block::ToolCall { name, done: true, output_preview, is_error: false, .. }
                if name == "read" && output_preview == "line one"
        ));
        assert!(matches!(&t.blocks[3], Block::AssistantText { text, .. } if text == "fixed"));
        assert!(matches!(&t.blocks[4], Block::Notice { .. }));
        assert_eq!(t.usage.input_tokens, 100);
        assert_eq!(t.turns, 2);
        assert!(!t.running);
    }

    #[test]
    fn profile_change_renders_notice_and_run_finished_is_cumulative() {
        let mut t = Transcript::default();
        t.apply(&SessionEvent::ProfileChanged { label: "OpenAI · gpt-5.2".into() });
        assert!(matches!(&t.blocks[0], Block::Notice { text } if text.contains("gpt-5.2")));
        t.apply(&SessionEvent::RunFinished {
            final_text: "ok".into(),
            turns: 3,
            usage: Usage { input_tokens: 500, ..Default::default() },
            stop_reason: "end_turn".into(),
        });
        // Sessions report cumulative usage; the transcript stores it as-is.
        assert_eq!(t.usage.input_tokens, 500);
        assert_eq!(t.turns, 3);
    }
}
