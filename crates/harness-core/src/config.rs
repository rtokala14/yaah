//! Provider configuration. Every provider is user-configurable: base URL,
//! auth, model, and — per the design requirement — arbitrary extra HTTP
//! headers and extra top-level request-body fields, so any OpenAI-compatible
//! server or gateway quirk can be accommodated without code changes.

use crate::types::Effort;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Anthropic,
    OpenAi,
    OpenAiCompat,
}

impl ProviderKind {
    pub fn label(&self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "Anthropic",
            ProviderKind::OpenAi => "OpenAI",
            ProviderKind::OpenAiCompat => "OpenAI-compatible",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Display name ("Anthropic", "Local vLLM", ...). Unique per profile.
    pub name: String,
    pub kind: ProviderKind,
    /// Default is filled in per kind when empty.
    #[serde(default)]
    pub base_url: String,
    /// Literal API key. Prefer `api_key_env`.
    #[serde(default)]
    pub api_key: String,
    /// Environment variable to read the key from (takes precedence when set).
    #[serde(default)]
    pub api_key_env: String,
    pub model: String,
    #[serde(default)]
    pub effort: Option<Effort>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// Extra HTTP headers sent on every request (e.g. `anthropic-beta`,
    /// gateway auth, org routing).
    #[serde(default)]
    pub extra_headers: Vec<(String, String)>,
    /// Extra top-level JSON fields merged into every request body (e.g.
    /// `{"reasoning_effort": "high"}`, vLLM sampling extensions). Wins over
    /// harness-generated fields on key collision.
    #[serde(default)]
    pub extra_body: Map<String, Value>,
    /// OpenAI-compat quirk switches.
    #[serde(default)]
    pub supports_reasoning_effort: bool,
}

fn default_max_tokens() -> u32 {
    16_000
}

impl ProviderConfig {
    pub fn resolved_base_url(&self) -> String {
        if !self.base_url.is_empty() {
            return self.base_url.trim_end_matches('/').to_string();
        }
        match self.kind {
            ProviderKind::Anthropic => "https://api.anthropic.com".into(),
            ProviderKind::OpenAi => "https://api.openai.com/v1".into(),
            ProviderKind::OpenAiCompat => String::new(),
        }
    }

    pub fn resolved_api_key(&self) -> String {
        if !self.api_key_env.is_empty() {
            if let Ok(v) = std::env::var(&self.api_key_env) {
                return v;
            }
        }
        if !self.api_key.is_empty() {
            return self.api_key.clone();
        }
        // conventional fallbacks
        let var = match self.kind {
            ProviderKind::Anthropic => "ANTHROPIC_API_KEY",
            _ => "OPENAI_API_KEY",
        };
        std::env::var(var).unwrap_or_default()
    }

    pub fn default_profiles() -> Vec<ProviderConfig> {
        vec![
            ProviderConfig {
                name: "Anthropic".into(),
                kind: ProviderKind::Anthropic,
                base_url: String::new(),
                api_key: String::new(),
                api_key_env: "ANTHROPIC_API_KEY".into(),
                model: "claude-opus-5".into(),
                effort: Some(Effort::XHigh),
                temperature: None,
                max_tokens: 16_000,
                extra_headers: vec![],
                extra_body: Map::new(),
                supports_reasoning_effort: false,
            },
            ProviderConfig {
                name: "OpenAI".into(),
                kind: ProviderKind::OpenAi,
                base_url: String::new(),
                api_key: String::new(),
                api_key_env: "OPENAI_API_KEY".into(),
                model: "gpt-5.2".into(),
                effort: Some(Effort::High),
                temperature: None,
                max_tokens: 16_000,
                extra_headers: vec![],
                extra_body: Map::new(),
                supports_reasoning_effort: true,
            },
            ProviderConfig {
                name: "Local (OpenAI-compatible)".into(),
                kind: ProviderKind::OpenAiCompat,
                base_url: "http://localhost:11434/v1".into(),
                api_key: "none".into(),
                api_key_env: String::new(),
                model: "qwen3:32b".into(),
                effort: None,
                temperature: None,
                max_tokens: 8_000,
                extra_headers: vec![],
                extra_body: Map::new(),
                supports_reasoning_effort: false,
            },
        ]
    }
}
