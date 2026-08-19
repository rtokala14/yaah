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

use crate::config::{ProviderKind, RunProfile};
use crate::http::post_json_streaming;
use crate::sse::SseReader;
use crate::types::*;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

/// One tool call being assembled from stream deltas.
#[derive(Default, Debug, PartialEq)]
struct PartialCall {
    id: String,
    name: String,
    args: String,
}

/// Whether `args` already parses as a complete JSON value. Used only to tell a
/// finished call from one still accumulating, when the server gives us no
/// `index` and no `id` to group by.
fn is_complete_json(args: &str) -> bool {
    let t = args.trim();
    !t.is_empty() && serde_json::from_str::<Value>(t).is_ok()
}

/// Reassembles `tool_calls` deltas into whole calls.
///
/// This exists as its own type because the delta protocol is under-specified
/// and every compat server bends it differently. Two failure modes have been
/// observed in the wild and both silently corrupt tool names, so they are
/// pinned by tests:
///
///  - **Missing `index`.** Defaulting it to 0 merges every call in the turn
///    into one, producing Frankenstein names like `readreadglobread`. When
///    there is no index we fall back to `id`, then to a name change.
///  - **Repeated `name`.** Many servers resend the *complete* function name in
///    every delta for a call. Blindly appending yields `readread`. Only a name
///    that differs from what we already hold is treated as a new fragment.
///
/// An explicit `index` is authoritative for grouping — except when it arrives
/// with a different `id` than the slot already holds, which means the server
/// is reusing indices across calls.
#[derive(Default)]
struct ToolCallAccumulator {
    calls: Vec<PartialCall>,
    by_index: BTreeMap<u64, usize>,
}

impl ToolCallAccumulator {
    fn new_slot(&mut self) -> usize {
        self.calls.push(PartialCall::default());
        self.calls.len() - 1
    }

    /// Pick the call this delta belongs to, allocating one if it starts a new call.
    fn slot_for(&mut self, index: Option<u64>, id: &str, name: &str) -> usize {
        if let Some(ix) = index {
            if let Some(&slot) = self.by_index.get(&ix) {
                // Same index, different id ⇒ the server reuses indices.
                let stale = !id.is_empty()
                    && !self.calls[slot].id.is_empty()
                    && self.calls[slot].id != id;
                if !stale {
                    return slot;
                }
                let slot = self.new_slot();
                self.by_index.insert(ix, slot);
                return slot;
            }
            let slot = self.new_slot();
            self.by_index.insert(ix, slot);
            return slot;
        }
        if !id.is_empty() {
            if let Some(slot) = self.calls.iter().position(|c| c.id == id) {
                return slot;
            }
            return self.new_slot();
        }
        // No index and no id. Two signals remain that a name starts a *new*
        // call rather than continuing the current one:
        //   - it differs from the name we already hold, or
        //   - the call we are filling already has complete JSON arguments,
        //     so it cannot still be growing (this is what separates a second
        //     `read` call from the tail of a fragmented `read`).
        match self.calls.last() {
            Some(last) if !name.is_empty() && !last.name.is_empty() => {
                if last.name != name || is_complete_json(&last.args) {
                    self.new_slot()
                } else {
                    self.calls.len() - 1
                }
            }
            None => self.new_slot(),
            _ => self.calls.len() - 1,
        }
    }

    /// Fold in one delta. Returns the tool name if this delta *starts* a call,
    /// so the caller can emit a `ToolCallStart` exactly once per call.
    fn push(&mut self, tc: &Value) -> Option<String> {
        let index = tc.get("index").and_then(|v| v.as_u64());
        let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let name = tc.pointer("/function/name").and_then(|v| v.as_str()).unwrap_or("");

        let slot = self.slot_for(index, id, name);
        let entry = &mut self.calls[slot];

        if entry.id.is_empty() && !id.is_empty() {
            entry.id = id.to_string();
        }
        let mut started = None;
        if !name.is_empty() && entry.name != name {
            if entry.name.is_empty() {
                started = Some(name.to_string());
            }
            entry.name.push_str(name);
        }
        match tc.pointer("/function/arguments") {
            Some(Value::String(s)) => entry.args.push_str(s),
            // llama.cpp has shipped object-typed arguments
            Some(v @ Value::Object(_)) => entry.args = v.to_string(),
            _ => {}
        }
        started
    }

    fn into_calls(self) -> Vec<PartialCall> {
        self.calls
    }
}

pub struct OpenAiProvider {
    config: RunProfile,
    api_key: String,
}

impl OpenAiProvider {
    pub fn new(config: RunProfile) -> Result<Self, ProviderError> {
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
        let mut tool_calls = ToolCallAccumulator::default();
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
                    if let Some(name) = tool_calls.push(tc) {
                        on_delta(StreamDelta::ToolCallStart(name));
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
        let calls = tool_calls.into_calls();
        let n_calls = calls.len();
        for (i, call) in calls.into_iter().enumerate() {
            let PartialCall { id, name, args } = call;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Fold a stream of deltas and return (name, args) per assembled call.
    fn assemble(deltas: &[Value]) -> Vec<(String, String)> {
        let mut acc = ToolCallAccumulator::default();
        for d in deltas {
            acc.push(d);
        }
        acc.into_calls().into_iter().map(|c| (c.name, c.args)).collect()
    }

    fn delta(index: Option<u64>, id: Option<&str>, name: Option<&str>, args: Option<&str>) -> Value {
        let mut tc = Map::new();
        if let Some(i) = index {
            tc.insert("index".into(), json!(i));
        }
        if let Some(i) = id {
            tc.insert("id".into(), json!(i));
        }
        let mut f = Map::new();
        if let Some(n) = name {
            f.insert("name".into(), json!(n));
        }
        if let Some(a) = args {
            f.insert("arguments".into(), json!(a));
        }
        if !f.is_empty() {
            tc.insert("function".into(), Value::Object(f));
        }
        Value::Object(tc)
    }

    #[test]
    fn well_behaved_stream_splits_calls_and_concatenates_arguments() {
        let calls = assemble(&[
            delta(Some(0), Some("a"), Some("read"), Some("{\"path\"")),
            delta(Some(0), None, None, Some(":\"a.rs\"}")),
            delta(Some(1), Some("b"), Some("glob"), Some("{}")),
        ]);
        assert_eq!(
            calls,
            vec![
                ("read".to_string(), "{\"path\":\"a.rs\"}".to_string()),
                ("glob".to_string(), "{}".to_string()),
            ]
        );
    }

    #[test]
    fn repeated_full_name_in_every_delta_is_not_appended() {
        // Servers that resend the whole name each delta used to yield "readread".
        let calls = assemble(&[
            delta(Some(0), Some("a"), Some("read"), Some("{")),
            delta(Some(0), Some("a"), Some("read"), Some("}")),
        ]);
        assert_eq!(calls, vec![("read".to_string(), "{}".to_string())]);
    }

    #[test]
    fn fragmented_name_still_assembles() {
        let calls = assemble(&[
            delta(Some(0), Some("a"), Some("re"), None),
            delta(Some(0), None, Some("ad"), None),
        ]);
        assert_eq!(calls, vec![("read".to_string(), String::new())]);
    }

    /// The reported bug: no `index` on any delta. Every call collapsed into
    /// slot 0 and the names concatenated into `readreadglobread`.
    #[test]
    fn calls_without_index_are_kept_apart_by_id() {
        let calls = assemble(&[
            delta(None, Some("a"), Some("read"), Some("{}")),
            delta(None, Some("b"), Some("read"), Some("{}")),
            delta(None, Some("c"), Some("glob"), Some("{}")),
            delta(None, Some("d"), Some("read"), Some("{}")),
        ]);
        assert_eq!(calls.len(), 4, "got {calls:?}");
        assert!(calls.iter().all(|(n, _)| n == "read" || n == "glob"), "got {calls:?}");
        assert_eq!(calls[2].0, "glob");
    }

    /// The literal `readreadglobread` from the bug report: neither `index` nor
    /// `id`, four calls, one delta each. No name may ever be a concatenation
    /// of two tool names.
    #[test]
    fn four_bare_calls_do_not_concatenate_into_one_name() {
        let calls = assemble(&[
            delta(None, None, Some("read"), Some("{\"path\":\"a\"}")),
            delta(None, None, Some("read"), Some("{\"path\":\"b\"}")),
            delta(None, None, Some("glob"), Some("{\"q\":\"*\"}")),
            delta(None, None, Some("read"), Some("{\"path\":\"c\"}")),
        ]);
        assert_eq!(
            calls.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            vec!["read", "read", "glob", "read"],
            "got {calls:?}"
        );
        // Each call must carry only its own arguments.
        assert_eq!(calls[0].1, "{\"path\":\"a\"}");
        assert_eq!(calls[3].1, "{\"path\":\"c\"}");
    }

    /// Neither index nor id — a changed name is the only split signal left.
    #[test]
    fn calls_without_index_or_id_split_on_name_change() {
        let calls = assemble(&[
            delta(None, None, Some("read"), Some("{\"path\":\"a\"}")),
            delta(None, None, Some("glob"), Some("{\"q\":\"*\"}")),
        ]);
        assert_eq!(
            calls,
            vec![
                ("read".to_string(), "{\"path\":\"a\"}".to_string()),
                ("glob".to_string(), "{\"q\":\"*\"}".to_string()),
            ]
        );
    }

    #[test]
    fn reused_index_with_a_new_id_starts_a_new_call() {
        let calls = assemble(&[
            delta(Some(0), Some("a"), Some("read"), Some("{}")),
            delta(Some(0), Some("b"), Some("glob"), Some("{}")),
        ]);
        assert_eq!(
            calls,
            vec![("read".to_string(), "{}".to_string()), ("glob".to_string(), "{}".to_string())]
        );
    }

    #[test]
    fn object_typed_arguments_replace_rather_than_append() {
        let mut tc = Map::new();
        tc.insert("index".into(), json!(0));
        tc.insert("id".into(), json!("a"));
        tc.insert("function".into(), json!({"name": "read", "arguments": {"path": "a.rs"}}));
        let calls = assemble(&[Value::Object(tc)]);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "read");
        assert!(calls[0].1.contains("\"path\""), "got {:?}", calls[0].1);
    }

    #[test]
    fn tool_call_start_fires_once_per_call() {
        let mut acc = ToolCallAccumulator::default();
        let mut started = Vec::new();
        for d in [
            delta(Some(0), Some("a"), Some("read"), Some("{")),
            delta(Some(0), Some("a"), Some("read"), Some("}")),
            delta(Some(1), Some("b"), Some("glob"), Some("{}")),
        ] {
            if let Some(n) = acc.push(&d) {
                started.push(n);
            }
        }
        assert_eq!(started, vec!["read".to_string(), "glob".to_string()]);
    }
}
