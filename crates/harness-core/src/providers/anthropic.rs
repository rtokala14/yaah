//! Anthropic Messages API adapter (streaming).
//!
//! Cache strategy: one cache_control breakpoint on the system block (caches
//! tools + system) and one on the last message content block so each turn
//! extends the cached prefix incrementally. Bodies are built with stable key
//! order and no volatile fields.

use crate::config::ProviderConfig;
use crate::http::{post_json_streaming, truncate};
use crate::sse::SseReader;
use crate::types::*;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

pub struct AnthropicProvider {
    config: ProviderConfig,
    api_key: String,
}

impl AnthropicProvider {
    pub fn new(config: ProviderConfig) -> Result<Self, ProviderError> {
        let api_key = config.resolved_api_key();
        if api_key.is_empty() {
            return Err(ProviderError::Config(
                "no Anthropic API key (set api_key, api_key_env, or ANTHROPIC_API_KEY)".into(),
            ));
        }
        Ok(Self { config, api_key })
    }

    fn build_body(&self, req: &ProviderRequest) -> Value {
        let mut messages = render_messages(&req.messages);
        // Incremental caching: breakpoint on the last block of the last message.
        if let Some(Value::Object(last_msg)) = messages.last_mut() {
            if let Some(Value::Array(blocks)) = last_msg.get_mut("content") {
                if let Some(Value::Object(block)) = blocks.last_mut() {
                    let t = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    if t == "text" || t == "tool_result" {
                        block.insert("cache_control".into(), json!({"type": "ephemeral"}));
                    }
                }
            }
        }

        let mut body = Map::new();
        body.insert("model".into(), json!(self.config.model));
        body.insert("max_tokens".into(), json!(req.max_tokens));
        body.insert("stream".into(), json!(true));
        body.insert(
            "system".into(),
            json!([{
                "type": "text",
                "text": req.system,
                "cache_control": {"type": "ephemeral"}
            }]),
        );
        body.insert("messages".into(), Value::Array(messages));
        if !req.tools.is_empty() {
            let tools: Vec<Value> = req
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.input_schema,
                    })
                })
                .collect();
            body.insert("tools".into(), Value::Array(tools));
        }
        if let Some(effort) = req.effort {
            let level = match effort {
                Effort::Low => "low",
                Effort::Medium => "medium",
                Effort::High => "high",
                Effort::XHigh => "xhigh",
                Effort::Max => "max",
            };
            body.insert("output_config".into(), json!({ "effort": level }));
        }
        // User-supplied extra fields win on collision.
        for (k, v) in &self.config.extra_body {
            body.insert(k.clone(), v.clone());
        }
        Value::Object(body)
    }
}

impl Provider for AnthropicProvider {
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
        let url = format!("{}/v1/messages", self.config.resolved_base_url());
        let mut headers: Vec<(String, String)> = vec![
            ("x-api-key".into(), self.api_key.clone()),
            ("anthropic-version".into(), "2023-06-01".into()),
            ("accept".into(), "text/event-stream".into()),
        ];
        headers.extend(self.config.extra_headers.iter().cloned());

        let body = self.build_body(req);
        let res = post_json_streaming(&url, &headers, &body, cancel)?;

        let mut content: Vec<AssistantPart> = Vec::new();
        let mut stop_reason = StopReason::EndTurn;
        let mut usage = Usage::default();
        // per-index accumulation; BTreeMap keeps block order deterministic
        let mut partial: BTreeMap<u64, (AssistantPart, String)> = BTreeMap::new();

        for event in SseReader::new(res.reader) {
            if cancel.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }
            let event = event.map_err(|e| ProviderError::Stream(e.to_string()))?;
            if event.event == "ping" {
                continue;
            }
            let data: Value = match serde_json::from_str(&event.data) {
                Ok(v) => v,
                Err(_) => continue,
            };
            match data.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                "message_start" => {
                    if let Some(u) = data.pointer("/message/usage") {
                        usage.input_tokens = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                        usage.cache_read_tokens =
                            u.get("cache_read_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                        usage.cache_write_tokens =
                            u.get("cache_creation_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    }
                }
                "content_block_start" => {
                    let index = data.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                    let block = data.get("content_block").cloned().unwrap_or(Value::Null);
                    let btype = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    let part = match btype {
                        "text" => AssistantPart::Text { text: String::new() },
                        "thinking" => AssistantPart::Thinking {
                            text: String::new(),
                            signature: None,
                            raw: None,
                        },
                        "redacted_thinking" => AssistantPart::Thinking {
                            text: String::new(),
                            signature: None,
                            raw: Some(block.clone()),
                        },
                        "tool_use" => {
                            let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            on_delta(StreamDelta::ToolCallStart(name.clone()));
                            AssistantPart::ToolCall(ToolCallPart {
                                id: block.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                name,
                                input: json!({}),
                                raw_arguments: None,
                            })
                        }
                        _ => continue,
                    };
                    partial.insert(index, (part, String::new()));
                }
                "content_block_delta" => {
                    let index = data.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                    let Some((part, args)) = partial.get_mut(&index) else { continue };
                    let delta = data.get("delta").cloned().unwrap_or(Value::Null);
                    match delta.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                        "text_delta" => {
                            if let AssistantPart::Text { text } = part {
                                let d = delta.get("text").and_then(|v| v.as_str()).unwrap_or("");
                                text.push_str(d);
                                on_delta(StreamDelta::Text(d.to_string()));
                            }
                        }
                        "thinking_delta" => {
                            if let AssistantPart::Thinking { text, .. } = part {
                                let d = delta.get("thinking").and_then(|v| v.as_str()).unwrap_or("");
                                text.push_str(d);
                                on_delta(StreamDelta::Thinking(d.to_string()));
                            }
                        }
                        "signature_delta" => {
                            if let AssistantPart::Thinking { signature, .. } = part {
                                let d = delta.get("signature").and_then(|v| v.as_str()).unwrap_or("");
                                *signature = Some(signature.take().unwrap_or_default() + d);
                            }
                        }
                        "input_json_delta" => {
                            args.push_str(delta.get("partial_json").and_then(|v| v.as_str()).unwrap_or(""));
                        }
                        _ => {}
                    }
                }
                "content_block_stop" => {
                    let index = data.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                    if let Some((mut part, args)) = partial.remove(&index) {
                        if let AssistantPart::ToolCall(call) = &mut part {
                            call.raw_arguments = Some(args.clone());
                            call.input = serde_json::from_str(&args).unwrap_or(json!({}));
                        }
                        content.push(part);
                    }
                }
                "message_delta" => {
                    if let Some(r) = data.pointer("/delta/stop_reason").and_then(|v| v.as_str()) {
                        stop_reason = map_stop_reason(r);
                    }
                    if let Some(o) = data.pointer("/usage/output_tokens").and_then(|v| v.as_u64()) {
                        usage.output_tokens = o;
                    }
                }
                "error" => {
                    return Err(ProviderError::Stream(truncate(&event.data, 500)));
                }
                _ => {}
            }
        }

        Ok(AssistantTurn { content, stop_reason, usage })
    }
}

fn map_stop_reason(r: &str) -> StopReason {
    match r {
        "tool_use" => StopReason::ToolUse,
        "max_tokens" | "model_context_window_exceeded" => StopReason::MaxTokens,
        "refusal" => StopReason::Refusal,
        _ => StopReason::EndTurn,
    }
}

/// Render internal messages into Anthropic wire format. Consecutive user-role
/// content is merged into a single message (system notes render as tagged
/// user text — portable and cache-safe).
fn render_messages(messages: &[AgentMessage]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();

    let push_user = |out: &mut Vec<Value>, blocks: Vec<Value>| {
        if let Some(Value::Object(last)) = out.last_mut() {
            if last.get("role").and_then(|v| v.as_str()) == Some("user") {
                if let Some(Value::Array(content)) = last.get_mut("content") {
                    content.extend(blocks);
                    return;
                }
            }
        }
        out.push(json!({"role": "user", "content": blocks}));
    };

    for m in messages {
        match m {
            AgentMessage::SystemNote { content } => {
                push_user(
                    &mut out,
                    vec![json!({"type": "text", "text": format!("<system-note>\n{content}\n</system-note>")})],
                );
            }
            AgentMessage::User { content } => {
                let blocks: Vec<Value> = content
                    .iter()
                    .map(|p| match p {
                        UserPart::Text { text } => json!({"type": "text", "text": text}),
                        UserPart::Image { media_type, data } => json!({
                            "type": "image",
                            "source": {"type": "base64", "media_type": media_type, "data": data}
                        }),
                        UserPart::ToolResult(r) => {
                            let mut v = json!({
                                "type": "tool_result",
                                "tool_use_id": r.tool_call_id,
                                "content": r.content,
                            });
                            if r.is_error {
                                v["is_error"] = json!(true);
                            }
                            v
                        }
                    })
                    .collect();
                push_user(&mut out, blocks);
            }
            AgentMessage::Assistant { content } => {
                let blocks: Vec<Value> = content
                    .iter()
                    .map(|p| match p {
                        AssistantPart::Text { text } => json!({"type": "text", "text": text}),
                        AssistantPart::Thinking { text, signature, raw } => {
                            // Redacted blocks round-trip via raw; normal blocks
                            // echo text+signature byte-identically.
                            if let Some(raw) = raw {
                                raw.clone()
                            } else {
                                json!({
                                    "type": "thinking",
                                    "thinking": text,
                                    "signature": signature.clone().unwrap_or_default(),
                                })
                            }
                        }
                        AssistantPart::ToolCall(c) => json!({
                            "type": "tool_use", "id": c.id, "name": c.name, "input": c.input
                        }),
                    })
                    .collect();
                out.push(json!({"role": "assistant", "content": blocks}));
            }
        }
    }
    out
}
