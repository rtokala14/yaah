//! Core data model. Anthropic-shaped superset; see lib.rs.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Content blocks

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserPart {
    Text { text: String },
    Image { media_type: String, data: String },
    ToolResult(ToolResultPart),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultPart {
    pub tool_call_id: String,
    pub tool_name: String,
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantPart {
    Text {
        text: String,
    },
    /// Model reasoning. `signature` is Anthropic's opaque integrity token;
    /// `raw` carries whatever an OpenAI-compatible server returned so the
    /// adapter can echo it back verbatim on replay.
    Thinking {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        raw: Option<Value>,
    },
    ToolCall(ToolCallPart),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallPart {
    pub id: String,
    pub name: String,
    pub input: Value,
    /// Original argument string as streamed, echoed back verbatim on replay.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_arguments: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum AgentMessage {
    User { content: Vec<UserPart> },
    Assistant { content: Vec<AssistantPart> },
    /// Harness-side steering injected mid-conversation. Rendered as a tagged
    /// block appended after the cached prefix — never by editing the system
    /// prompt (which would invalidate the prompt cache).
    SystemNote { content: String },
}

// ---------------------------------------------------------------------------
// Tools

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn ok(content: impl Into<String>) -> Self {
        Self { content: content.into(), is_error: false }
    }
    pub fn err(content: impl Into<String>) -> Self {
        Self { content: content.into(), is_error: true }
    }
}

/// Shared per-session tool state. `read_files` maps canonical path -> mtime
/// (as nanos) at last read, for edit staleness checks.
pub struct ToolContext {
    pub cwd: PathBuf,
    pub cancel: CancelToken,
    pub read_files: Mutex<HashMap<PathBuf, u128>>,
}

pub trait Tool: Send + Sync {
    fn def(&self) -> &ToolDef;
    /// Read-only tools may execute in parallel with each other.
    fn read_only(&self) -> bool;
    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolOutput;
}

// ---------------------------------------------------------------------------
// Provider abstraction

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Refusal,
    Error,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

impl Usage {
    pub fn add(&mut self, other: &Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.cache_write_tokens += other.cache_write_tokens;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

#[derive(Debug, Clone)]
pub struct ProviderRequest {
    /// Byte-stable system prompt. Keep identical across turns for caching.
    pub system: String,
    pub messages: Vec<AgentMessage>,
    pub tools: Vec<ToolDef>,
    pub max_tokens: u32,
    pub effort: Option<Effort>,
    pub temperature: Option<f64>,
}

/// Streaming callbacks; deltas for live UI rendering.
#[derive(Debug, Clone)]
pub enum StreamDelta {
    Text(String),
    Thinking(String),
    ToolCallStart(String),
}

#[derive(Debug, Clone)]
pub struct AssistantTurn {
    pub content: Vec<AssistantPart>,
    pub stop_reason: StopReason,
    pub usage: Usage,
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("http {status}: {body}")]
    Api { status: u16, body: String },
    #[error("network: {0}")]
    Network(String),
    #[error("stream: {0}")]
    Stream(String),
    #[error("cancelled")]
    Cancelled,
    #[error("config: {0}")]
    Config(String),
}

pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn model(&self) -> &str;
    fn stream(
        &self,
        req: &ProviderRequest,
        on_delta: &mut dyn FnMut(StreamDelta),
        cancel: &CancelToken,
    ) -> Result<AssistantTurn, ProviderError>;
}

// ---------------------------------------------------------------------------
// Cancellation

#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    /// Re-arm after an interrupt so the same session can run again.
    pub fn reset(&self) {
        self.0.store(false, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

// ---------------------------------------------------------------------------
// Loop events (for UIs / logging / the replay journal)

#[derive(Debug, Clone)]
pub enum AgentEvent {
    TurnStart { turn: u32 },
    TextDelta(String),
    ThinkingDelta(String),
    ToolStart { name: String, input: Value },
    ToolEnd { name: String, output_preview: String, is_error: bool, duration_ms: u64 },
    Compaction { before_tokens: usize },
    Pruned { count: usize },
    TurnEnd { stop_reason: StopReason, usage: Usage },
    Done { reason: String, final_text: String },
    Error(String),
}
