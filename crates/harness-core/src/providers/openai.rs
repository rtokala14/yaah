//! OpenAI / OpenAI-compatible chat-completions adapter (streaming).
//!
//! One adapter serves both `ProviderKind::OpenAi` and `OpenAiCompat`; the
//! difference is defaults (base URL, reasoning_effort support). Compat-server
//! quirks handled defensively:
//!  - tool-call deltas keyed by `index` with id fallback
//!  - `reasoning_content` (DeepSeek/Qwen/vLLM) and `reasoning` both parsed
//!  - `arguments` tolerated as object (llama.cpp regression) or string
//!  - `finish_reason: stop` despite tool calls → accumulated calls win
//!  - usage requested via stream_options but tolerated if absent

use crate::config::{ProviderConfig, ProviderKind};
use crate::http::post_json_streaming;
use crate::sse::SseReader;
use crate::types::*;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

pub struct OpenAiProvider {
    config: ProviderConfig,
    api_key: String,
}

impl OpenAiProvider {
    pub fn new(config: ProviderConfig) -> Result<Self, ProviderError> {
        if config.resolved_base_url().is_empty() {
            return Err(ProviderError::Config("base_url is required".into()));
        }
        let api_key = config.resolved_api_key();
        Ok(Self { config, api_key })
    }

    fn build_body(&self, req: &ProviderRequest) -> Value {
        let mut body = Map::new();
        body.insert("model".into(), json!(self.config.model));
        body.insert("stream".into(), json!(true));
        body.insert("stream_options".into(), json!({"include_usage": true}));
        body.insert("max_tokens".into(), json!(req.max_tokens));
        body.insert("messages".into(), Value::Array(render_messages(&req.system, &req.messages)));
        if !req.tools.is_empty() {
            let tools: Vec<Value> = req
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.input_schema,
                        }
                    })
                })
                .collect();
            body.insert("tools".into(), Value::Array(tools));
        }
        if let Some(temp) = req.temperature {
            body.insert("temperature".into(), json!(temp));
        }
        if self.config.supports_reasoning_effort || self.config.kind == ProviderKind::OpenAi {
            if let Some(effort) = req.effort {
                // OpenAI ladder: none|minimal|low|medium|high|xhigh
                let level = match effort {
                    Effort::Low => "low",
                    Effort::Medium => "medium",
                    Effort::High => "high",
                    Effort::XHigh | Effort::Max => "xhigh",
                };
                body.insert("reasoning_effort".into(), json!(level));
            }
        }
        for (k, v) in &self.config.extra_body {
            body.insert(k.clone(), v.clone());
        }
        Value::Object(body)
    }
}

impl Provider for OpenAiProvider {
    fn name(&self) -> &str {
        &self.config.name
    }
    fn model(&self) -> &str {
        &self.config.model
    }

    fn stream(
        &self,
        req: &ProviderRequest,
        on_delta: &mut dyn FnMut(StreamDelta),
        cancel: &CancelToken,
    ) -> Result<AssistantTurn, ProviderError> {
        let url = format!("{}/chat/completions", self.config.resolved_base_url());
        let mut headers: Vec<(String, String)> = vec![
            ("authorization".into(), format!("Bearer {}", self.api_key)),
            ("accept".into(), "text/event-stream".into()),
        ];
        headers.extend(self.config.extra_headers.iter().cloned());

        let body = self.build_body(req);
        let res = post_json_streaming(&url, &headers, &body, cancel)?;

        let mut text = String::new();
        let mut reasoning = String::new();
        // index -> (id, name, args)
        let mut tool_calls: BTreeMap<u64, (String, String, String)> = BTreeMap::new();
        let mut finish_reason: Option<String> = None;
        let mut usage = Usage::default();

        for event in SseReader::new(res.reader) {
            if cancel.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }
            let event = event.map_err(|e| ProviderError::Stream(e.to_string()))?;
            if event.data.trim() == "[DONE]" {
                break;
            }
            let chunk: Value = match serde_json::from_str(&event.data) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if let Some(u) = chunk.get("usage").filter(|u| !u.is_null()) {
                usage.input_tokens = u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                usage.output_tokens = u.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                usage.cache_read_tokens = u
                    .pointer("/prompt_tokens_details/cached_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
            }
            let Some(choice) = chunk.pointer("/choices/0") else { continue };
            if let Some(fr) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                finish_reason = Some(fr.to_string());
            }
            let delta = choice.get("delta").cloned().unwrap_or(Value::Null);
            if let Some(d) = delta.get("content").and_then(|v| v.as_str()) {
                if !d.is_empty() {
                    text.push_str(d);
                    on_delta(StreamDelta::Text(d.to_string()));
                }
            }
            let r = delta
                .get("reasoning_content")
                .or_else(|| delta.get("reasoning"))
                .and_then(|v| v.as_str());
            if let Some(d) = r {
                if !d.is_empty() {
                    reasoning.push_str(d);
                    on_delta(StreamDelta::Thinking(d.to_string()));
                }
            }
            if let Some(Value::Array(deltas)) = delta.get("tool_calls") {
                for tc in deltas {
                    let index = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                    let entry = tool_calls.entry(index).or_default();
                    if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                        if entry.0.is_empty() {
                            entry.0 = id.to_string();
                        }
                    }
                    if let Some(name) = tc.pointer("/function/name").and_then(|v| v.as_str()) {
                        if entry.1.is_empty() {
                            on_delta(StreamDelta::ToolCallStart(name.to_string()));
                        }
                        entry.1.push_str(name);
                    }
                    match tc.pointer("/function/arguments") {
                        Some(Value::String(s)) => entry.2.push_str(s),
                        // llama.cpp has shipped object-typed arguments
                        Some(v @ Value::Object(_)) => entry.2 = v.to_string(),
                        _ => {}
                    }
                }
            }
        }

        let mut content: Vec<AssistantPart> = Vec::new();
        if !reasoning.is_empty() {
            content.push(AssistantPart::Thinking {
                text: reasoning.clone(),
                signature: None,
                raw: Some(json!({"reasoning_content": reasoning})),
            });
        }
        if !text.is_empty() {
            content.push(AssistantPart::Text { text });
        }
        let n_calls = tool_calls.len();
        for (i, (_, (id, name, args))) in tool_calls.into_iter().enumerate() {
            let id = if id.is_empty() { format!("call_{i}") } else { id };
            content.push(AssistantPart::ToolCall(ToolCallPart {
                id,
                name,
                input: serde_json::from_str(&args).unwrap_or(json!({})),
                raw_arguments: Some(args),
            }));
        }

        let stop_reason = if n_calls > 0 || finish_reason.as_deref() == Some("tool_calls") {
            StopReason::ToolUse
        } else {
            match finish_reason.as_deref() {
                Some("length") => StopReason::MaxTokens,
                Some("content_filter") => StopReason::Refusal,
                _ => StopReason::EndTurn,
            }
        };

        Ok(AssistantTurn { content, stop_reason, usage })
    }
}

/// Render internal messages into chat-completions wire format.
fn render_messages(system: &str, messages: &[AgentMessage]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    if !system.is_empty() {
        out.push(json!({"role": "system", "content": system}));
    }
    for m in messages {
        match m {
            AgentMessage::SystemNote { content } => {
                // Mid-conversation `system` role support varies wildly across
                // compat servers; tagged user text is the portable channel.
                out.push(json!({
                    "role": "user",
                    "content": format!("<system-note>\n{content}\n</system-note>")
                }));
            }
            AgentMessage::User { content } => {
                let mut user_parts: Vec<Value> = Vec::new();
                let mut only_text = true;
                for p in content {
                    match p {
                        UserPart::ToolResult(r) => {
                            out.push(json!({
                                "role": "tool",
                                "tool_call_id": r.tool_call_id,
                                "content": if r.is_error {
                                    format!("ERROR: {}", r.content)
                                } else {
                                    r.content.clone()
                                },
                            }));
                        }
                        UserPart::Text { text } => {
                            user_parts.push(json!({"type": "text", "text": text}));
                        }
                        UserPart::Image { media_type, data } => {
                            only_text = false;
                            user_parts.push(json!({
                                "type": "image_url",
                                "image_url": {"url": format!("data:{media_type};base64,{data}")}
                            }));
                        }
                    }
                }
                if !user_parts.is_empty() {
                    // Plain string when text-only: older servers choke on arrays.
                    let content = if only_text {
                        Value::String(
                            user_parts
                                .iter()
                                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                                .collect::<Vec<_>>()
                                .join(""),
                        )
                    } else {
                        Value::Array(user_parts)
                    };
                    out.push(json!({"role": "user", "content": content}));
                }
            }
            AgentMessage::Assistant { content } => {
                let mut text = String::new();
                let mut reasoning = String::new();
                let mut calls: Vec<Value> = Vec::new();
                for p in content {
                    match p {
                        AssistantPart::Text { text: t } => text.push_str(t),
                        AssistantPart::Thinking { text: t, .. } => reasoning.push_str(t),
                        AssistantPart::ToolCall(c) => calls.push(json!({
                            "type": "function",
                            "id": c.id,
                            "function": {
                                "name": c.name,
                                "arguments": c.raw_arguments.clone()
                                    .unwrap_or_else(|| c.input.to_string()),
                            }
                        })),
                    }
                }
                let mut msg = Map::new();
                msg.insert("role".into(), json!("assistant"));
                msg.insert(
                    "content".into(),
                    if text.is_empty() { Value::Null } else { Value::String(text) },
                );
                if !reasoning.is_empty() {
                    // Echo for servers that expect it (DeepSeek-style templates);
                    // servers that don't know the field ignore it.
                    msg.insert("reasoning_content".into(), json!(reasoning));
                }
                if !calls.is_empty() {
                    msg.insert("tool_calls".into(), Value::Array(calls));
                }
                out.push(Value::Object(msg));
            }
        }
    }
    out
}
