//! App settings: provider registry (providers own multiple models) + recent
//! projects, persisted as TOML in the platform config dir
//! (~/.config/blurb/settings.toml on Linux, ~/Library/Application
//! Support/blurb/ on macOS). The file is persistence, not an interface —
//! every field is editable in-app (settings overlay); legacy flat-format
//! files (provider-level `model`/`effort`) migrate on load.

use harness_core::config::{ProviderConfig, ProviderKind, RunProfile};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Settings {
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    /// Index into `providers` of the default provider (its `active_model`
    /// picks the model).
    #[serde(default)]
    pub active_provider: usize,
    #[serde(default)]
    pub recent_projects: Vec<PathBuf>,
    /// Isolate each session in its own git worktree + branch.
    #[serde(default = "default_true")]
    pub sessions_use_worktrees: bool,
}

fn default_true() -> bool {
    true
}

impl Settings {
    pub fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("blurb")
            .join("settings.toml")
    }

    pub fn load() -> Self {
        let settings = std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default();
        Self::from_parsed(settings)
    }

    /// Normalize a freshly-deserialized (or default) settings value:
    /// default providers when empty, legacy-field migration, index clamps.
    fn from_parsed(mut settings: Settings) -> Self {
        if settings.providers.is_empty() {
            settings.providers = ProviderConfig::default_profiles();
        }
        for p in &mut settings.providers {
            p.normalize();
        }
        settings.active_provider = settings.active_provider.min(settings.providers.len() - 1);
        settings
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn active(&self) -> &ProviderConfig {
        &self.providers[self.active_provider.min(self.providers.len() - 1)]
    }

    /// Resolved provider+model profile new sessions run against.
    pub fn active_profile(&self) -> RunProfile {
        self.active()
            .resolve_active()
            .expect("normalize() guarantees every provider has a model")
    }

    /// Select provider `p` (and optionally one of its models) as active.
    pub fn select(&mut self, provider: usize, model: Option<usize>) {
        if provider >= self.providers.len() {
            return;
        }
        self.active_provider = provider;
        if let Some(m) = model {
            let p = &mut self.providers[provider];
            p.active_model = m.min(p.models.len().saturating_sub(1));
        }
    }

    /// Add a new blank provider; returns its index.
    pub fn add_provider(&mut self, kind: ProviderKind) -> usize {
        let base = match kind {
            ProviderKind::Anthropic => "Anthropic",
            ProviderKind::OpenAi => "OpenAI",
            ProviderKind::OpenAiCompat => "Custom endpoint",
        };
        let name = self.unique_name(base);
        self.providers.push(ProviderConfig::new(name, kind));
        self.providers.len() - 1
    }

    pub fn duplicate_provider(&mut self, index: usize) -> Option<usize> {
        let mut copy = self.providers.get(index)?.clone();
        copy.name = self.unique_name(&format!("{} copy", copy.name));
        self.providers.insert(index + 1, copy);
        if self.active_provider > index {
            self.active_provider += 1;
        }
        Some(index + 1)
    }

    /// Remove a provider; the registry never becomes empty.
    pub fn remove_provider(&mut self, index: usize) {
        if index >= self.providers.len() {
            return;
        }
        self.providers.remove(index);
        if self.providers.is_empty() {
            self.providers = ProviderConfig::default_profiles();
        }
        if self.active_provider >= self.providers.len()
            || (self.active_provider >= index && self.active_provider > 0)
        {
            self.active_provider =
                self.active_provider.saturating_sub(1).min(self.providers.len() - 1);
        }
    }

    fn unique_name(&self, base: &str) -> String {
        if !self.providers.iter().any(|p| p.name == base) {
            return base.to_string();
        }
        (2..)
            .map(|n| format!("{base} {n}"))
            .find(|c| !self.providers.iter().any(|p| p.name == *c))
            .unwrap()
    }

    pub fn remember_project(&mut self, path: PathBuf) {
        self.recent_projects.retain(|p| p != &path);
        self.recent_projects.insert(0, path);
        self.recent_projects.truncate(10);
    }
}

/// Text ⇄ config conversions for the in-app provider editor. GPUI-free so
/// they can be unit-tested headless.
pub mod form {
    use serde_json::{Map, Value};

    /// Parse "Header-Name: value" lines (also accepts `=` as separator).
    /// Blank lines are skipped; a line without a separator is an error.
    pub fn parse_headers(text: &str) -> Result<Vec<(String, String)>, String> {
        let mut out = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let (k, v) = line
                .split_once(':')
                .or_else(|| line.split_once('='))
                .ok_or_else(|| format!("header line needs `name: value` — got \"{line}\""))?;
            let k = k.trim();
            if k.is_empty() {
                return Err(format!("empty header name in \"{line}\""));
            }
            out.push((k.to_string(), v.trim().to_string()));
        }
        Ok(out)
    }

    pub fn format_headers(headers: &[(String, String)]) -> String {
        headers.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("\n")
    }

    /// Parse a JSON object (or empty text → empty map).
    pub fn parse_json_object(text: &str) -> Result<Map<String, Value>, String> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(Map::new());
        }
        match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(m)) => Ok(m),
            Ok(_) => Err("extra body must be a JSON object, e.g. {\"top_k\": 20}".into()),
            Err(e) => Err(format!("invalid JSON: {e}")),
        }
    }

    pub fn format_json_object(map: &Map<String, Value>) -> String {
        if map.is_empty() {
            String::new()
        } else {
            serde_json::to_string_pretty(&Value::Object(map.clone())).unwrap_or_default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_flat_settings_file_migrates() {
        let legacy = r#"
            active_provider = 1

            [[providers]]
            name = "Anthropic"
            kind = "anthropic"
            api_key_env = "ANTHROPIC_API_KEY"
            model = "claude-opus-5"
            effort = "xhigh"
            extra_headers = [["anthropic-beta", "context-management-2025-06-27"]]

            [[providers]]
            name = "Local vLLM"
            kind = "openai_compat"
            base_url = "http://localhost:8000/v1"
            model = "qwen3-coder"
            [providers.extra_body]
            top_k = 20
        "#;
        let parsed: Settings = toml::from_str(legacy).unwrap();
        let s = Settings::from_parsed(parsed);
        assert_eq!(s.providers.len(), 2);
        assert_eq!(s.providers[0].models.len(), 1);
        assert_eq!(s.providers[0].models[0].id, "claude-opus-5");
        assert_eq!(s.providers[1].models[0].id, "qwen3-coder");
        let profile = s.active_profile();
        assert_eq!(profile.model, "qwen3-coder");
        assert_eq!(profile.extra_body.get("top_k"), Some(&serde_json::json!(20)));
        // Saved form is the new multi-model shape.
        let out = toml::to_string_pretty(&s).unwrap();
        assert!(out.contains("[[providers.models]]"));
    }

    #[test]
    fn new_format_round_trips() {
        let s = Settings::from_parsed(Settings::default());
        let out = toml::to_string_pretty(&s).unwrap();
        let back = Settings::from_parsed(toml::from_str(&out).unwrap());
        assert_eq!(back.providers.len(), s.providers.len());
        assert_eq!(back.providers[0].models.len(), s.providers[0].models.len());
    }

    #[test]
    fn provider_crud_keeps_invariants() {
        let mut s = Settings::from_parsed(Settings::default());
        let n = s.providers.len();
        let i = s.add_provider(ProviderKind::OpenAiCompat);
        assert_eq!(s.providers.len(), n + 1);
        assert!(!s.providers[i].models.is_empty());

        let dup = s.duplicate_provider(0).unwrap();
        assert_ne!(s.providers[dup].name, s.providers[0].name);

        s.select(i + 1, Some(0)); // index shifted by the duplicate
        for k in (0..s.providers.len()).rev() {
            s.remove_provider(k);
        }
        // Registry refuses to be empty.
        assert!(!s.providers.is_empty());
        assert!(s.active_provider < s.providers.len());
        let _ = s.active_profile();
    }

    #[test]
    fn select_clamps_model_index() {
        let mut s = Settings::from_parsed(Settings::default());
        s.select(0, Some(999));
        assert!(s.providers[0].active_model < s.providers[0].models.len());
    }

    #[test]
    fn header_form_round_trips() {
        let parsed =
            form::parse_headers("anthropic-beta: foo\n\nx-org = bar\n").unwrap();
        assert_eq!(
            parsed,
            vec![
                ("anthropic-beta".to_string(), "foo".to_string()),
                ("x-org".to_string(), "bar".to_string())
            ]
        );
        assert_eq!(form::format_headers(&parsed), "anthropic-beta: foo\nx-org: bar");
        assert!(form::parse_headers("no-separator-here").is_err());
    }

    #[test]
    fn json_object_form() {
        assert!(form::parse_json_object("  ").unwrap().is_empty());
        let m = form::parse_json_object(r#"{"top_k": 20}"#).unwrap();
        assert_eq!(m.get("top_k"), Some(&serde_json::json!(20)));
        assert!(form::parse_json_object("[1,2]").is_err());
        assert!(form::parse_json_object("{oops").is_err());
        let back = form::parse_json_object(&form::format_json_object(&m)).unwrap();
        assert_eq!(back, m);
    }
}
