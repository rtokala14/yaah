//! Provider configuration, v2: a provider is a *connection* (endpoint, auth,
//! headers, body quirks) that owns any number of *models*. The UI edits
//! `ProviderConfig`/`ModelConfig`; the session layer only ever consumes a
//! resolved flat `RunProfile` (one provider + one model).
//!
//! Everything is user-configurable — per the design requirement, arbitrary
//! extra HTTP headers and extra top-level request-body fields exist at both
//! the provider level (every request) and the model level (merged on top),
//! so any OpenAI-compatible server or gateway quirk is a settings entry,
//! not a code change.

use crate::types::Effort;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Anthropic,
    // snake_case would give "open_ai(_compat)"; settings files use the
    // documented "openai"/"openai_compat" (old spellings still accepted).
    #[serde(rename = "openai", alias = "open_ai")]
    OpenAi,
    #[serde(rename = "openai_compat", alias = "open_ai_compat")]
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

    pub const ALL: [ProviderKind; 3] =
        [ProviderKind::Anthropic, ProviderKind::OpenAi, ProviderKind::OpenAiCompat];
}

/// One model offered by a provider. `id` is what goes in the request body;
/// `label` is what the UI shows (falls back to `id` when empty).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub effort: Option<Effort>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// The model's context window in tokens; drives the session's context
    /// budget. None → a conservative default (200k-class assumption).
    #[serde(default)]
    pub context_window: Option<u32>,
    /// Extra top-level JSON fields for this model only; merged over the
    /// provider-level `extra_body` (model wins on key collision).
    #[serde(default)]
    pub extra_body: Map<String, Value>,
}

impl ModelConfig {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: String::new(),
            effort: None,
            temperature: None,
            max_tokens: default_max_tokens(),
            context_window: None,
            extra_body: Map::new(),
        }
    }

    pub fn display_label(&self) -> &str {
        if self.label.is_empty() {
            &self.id
        } else {
            &self.label
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Display name ("Anthropic", "Local vLLM", ...). Unique per provider.
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
    /// The models this provider offers. Never empty after `normalize()`.
    #[serde(default)]
    pub models: Vec<ModelConfig>,
    /// Index into `models` of this provider's selected model.
    #[serde(default)]
    pub active_model: usize,
    /// Extra HTTP headers sent on every request (e.g. `anthropic-beta`,
    /// gateway auth, org routing).
    #[serde(default)]
    pub extra_headers: Vec<(String, String)>,
    /// Extra top-level JSON fields merged into every request body. Wins over
    /// harness-generated fields on key collision; model-level extras win
    /// over these.
    #[serde(default)]
    pub extra_body: Map<String, Value>,
    /// OpenAI-compat quirk switches.
    #[serde(default)]
    pub supports_reasoning_effort: bool,

    // -- legacy flat-format fields (pre-multi-model settings files). Read on
    // load, folded into `models` by `normalize()`, never written back.
    #[doc(hidden)]
    #[serde(default, rename = "model", skip_serializing)]
    pub legacy_model: String,
    #[doc(hidden)]
    #[serde(default, rename = "effort", skip_serializing)]
    pub legacy_effort: Option<Effort>,
    #[doc(hidden)]
    #[serde(default, rename = "temperature", skip_serializing)]
    pub legacy_temperature: Option<f64>,
    #[doc(hidden)]
    #[serde(default, rename = "max_tokens", skip_serializing)]
    pub legacy_max_tokens: Option<u32>,
}

fn default_max_tokens() -> u32 {
    16_000
}

impl ProviderConfig {
    pub fn new(name: impl Into<String>, kind: ProviderKind) -> Self {
        Self {
            name: name.into(),
            kind,
            base_url: String::new(),
            api_key: String::new(),
            api_key_env: match kind {
                ProviderKind::Anthropic => "ANTHROPIC_API_KEY".into(),
                ProviderKind::OpenAi => "OPENAI_API_KEY".into(),
                ProviderKind::OpenAiCompat => String::new(),
            },
            models: vec![ModelConfig::new("")],
            active_model: 0,
            extra_headers: vec![],
            extra_body: Map::new(),
            supports_reasoning_effort: kind == ProviderKind::OpenAi,
            legacy_model: String::new(),
            legacy_effort: None,
            legacy_temperature: None,
            legacy_max_tokens: None,
        }
    }

    /// Repair invariants after deserialization or UI edits: fold legacy
    /// flat-format fields into `models`, guarantee at least one model, and
    /// clamp `active_model`.
    pub fn normalize(&mut self) {
        if !self.legacy_model.is_empty()
            && !self.models.iter().any(|m| m.id == self.legacy_model)
        {
            self.models.insert(
                0,
                ModelConfig {
                    id: std::mem::take(&mut self.legacy_model),
                    label: String::new(),
                    effort: self.legacy_effort.take(),
                    temperature: self.legacy_temperature.take(),
                    max_tokens: self.legacy_max_tokens.take().unwrap_or_else(default_max_tokens),
                    context_window: None,
                    extra_body: Map::new(),
                },
            );
            self.active_model = 0;
        }
        self.legacy_model = String::new();
        self.legacy_effort = None;
        self.legacy_temperature = None;
        self.legacy_max_tokens = None;
        if self.models.is_empty() {
            self.models.push(ModelConfig::new(""));
        }
        self.active_model = self.active_model.min(self.models.len() - 1);
    }

    pub fn active_model_config(&self) -> &ModelConfig {
        &self.models[self.active_model.min(self.models.len() - 1)]
    }

    /// Flatten this provider + one of its models into the profile the
    /// session layer consumes. Returns the active model's profile when
    /// `model_index` is out of range on a non-empty provider.
    pub fn resolve(&self, model_index: usize) -> Option<RunProfile> {
        let model = self.models.get(model_index).or_else(|| self.models.first())?;
        let mut extra_body = self.extra_body.clone();
        for (k, v) in &model.extra_body {
            extra_body.insert(k.clone(), v.clone());
        }
        Some(RunProfile {
            name: self.name.clone(),
            kind: self.kind,
            base_url: self.base_url.clone(),
            api_key: self.api_key.clone(),
            api_key_env: self.api_key_env.clone(),
            model: model.id.clone(),
            model_label: model.display_label().to_string(),
            effort: model.effort,
            temperature: model.temperature,
            max_tokens: model.max_tokens,
            context_window: model.context_window,
            extra_headers: self.extra_headers.clone(),
            extra_body,
            supports_reasoning_effort: self.supports_reasoning_effort,
        })
    }

    pub fn resolve_active(&self) -> Option<RunProfile> {
        self.resolve(self.active_model)
    }

    pub fn default_profiles() -> Vec<ProviderConfig> {
        let mut anthropic = ProviderConfig::new("Anthropic", ProviderKind::Anthropic);
        anthropic.models = vec![
            ModelConfig {
                effort: Some(Effort::XHigh),
                ..ModelConfig::new("claude-opus-5")
            },
            ModelConfig {
                effort: Some(Effort::High),
                ..ModelConfig::new("claude-sonnet-5")
            },
            ModelConfig {
                effort: Some(Effort::Medium),
                max_tokens: 8_000,
                ..ModelConfig::new("claude-haiku-4-5-20251001")
            },
        ];

        let mut openai = ProviderConfig::new("OpenAI", ProviderKind::OpenAi);
        openai.models = vec![
            ModelConfig { effort: Some(Effort::High), ..ModelConfig::new("gpt-5.2") },
            ModelConfig {
                effort: Some(Effort::Medium),
                max_tokens: 8_000,
                ..ModelConfig::new("gpt-5.2-mini")
            },
        ];

        let mut local = ProviderConfig::new("Local (OpenAI-compatible)", ProviderKind::OpenAiCompat);
        local.base_url = "http://localhost:11434/v1".into();
        local.api_key = "none".into();
        local.models = vec![ModelConfig { max_tokens: 8_000, ..ModelConfig::new("qwen3:32b") }];

        vec![anthropic, openai, local]
    }
}

/// The flat, resolved (provider + model) profile the adapters and session
/// threads consume. Built via `ProviderConfig::resolve`; `extra_body` here
/// is already the provider∪model merge.
#[derive(Debug, Clone)]
pub struct RunProfile {
    pub name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    pub api_key: String,
    pub api_key_env: String,
    pub model: String,
    pub model_label: String,
    pub effort: Option<Effort>,
    pub temperature: Option<f64>,
    pub max_tokens: u32,
    pub context_window: Option<u32>,
    pub extra_headers: Vec<(String, String)>,
    pub extra_body: Map<String, Value>,
    pub supports_reasoning_effort: bool,
}

impl RunProfile {
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

    /// "Provider · model" label for sidebars and session rows.
    pub fn label(&self) -> String {
        format!("{} · {}", self.name, self.model_label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolve_merges_extra_body_model_wins() {
        let mut p = ProviderConfig::new("vllm", ProviderKind::OpenAiCompat);
        p.extra_body.insert("top_k".into(), json!(20));
        p.extra_body.insert("min_p".into(), json!(0.05));
        p.models = vec![ModelConfig {
            extra_body: [("top_k".to_string(), json!(40))].into_iter().collect(),
            ..ModelConfig::new("qwen3-coder")
        }];
        let r = p.resolve(0).unwrap();
        assert_eq!(r.extra_body.get("top_k"), Some(&json!(40)));
        assert_eq!(r.extra_body.get("min_p"), Some(&json!(0.05)));
        assert_eq!(r.model, "qwen3-coder");
    }

    #[test]
    fn resolve_out_of_range_falls_back_to_first_model() {
        let mut p = ProviderConfig::new("a", ProviderKind::Anthropic);
        p.models = vec![ModelConfig::new("m1"), ModelConfig::new("m2")];
        assert_eq!(p.resolve(7).unwrap().model, "m1");
    }

    #[test]
    fn normalize_migrates_legacy_flat_fields() {
        // A pre-multi-model settings entry: provider-level model/effort.
        let legacy = json!({
            "name": "Anthropic",
            "kind": "anthropic",
            "model": "claude-opus-5",
            "effort": "xhigh",
            "max_tokens": 12000
        });
        let mut p: ProviderConfig = serde_json::from_value(legacy).unwrap();
        p.normalize();
        assert_eq!(p.models.len(), 1);
        let m = &p.models[0];
        assert_eq!(m.id, "claude-opus-5");
        assert_eq!(m.effort, Some(Effort::XHigh));
        assert_eq!(m.max_tokens, 12_000);
        assert_eq!(p.active_model, 0);
        // Round-trips without legacy keys.
        let v = serde_json::to_value(&p).unwrap();
        assert!(v.get("model").is_none());
        assert!(v.get("models").is_some());
    }

    #[test]
    fn normalize_guarantees_a_model_and_clamps_active() {
        let mut p = ProviderConfig::new("x", ProviderKind::OpenAi);
        p.models.clear();
        p.active_model = 5;
        p.normalize();
        assert_eq!(p.models.len(), 1);
        assert_eq!(p.active_model, 0);
    }

    #[test]
    fn model_display_label_falls_back_to_id() {
        let mut m = ModelConfig::new("gpt-5.2");
        assert_eq!(m.display_label(), "gpt-5.2");
        m.label = "GPT 5.2".into();
        assert_eq!(m.display_label(), "GPT 5.2");
    }
}
