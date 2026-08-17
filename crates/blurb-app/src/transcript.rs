//! Render-ready transcript state, built by folding `SessionEvent`s.
//! GPUI-free so it can be unit-tested; the chat view just draws it.

use harness_core::types::{AgentEvent, StopReason, Usage};
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
}
