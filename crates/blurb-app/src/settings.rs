//! App settings: provider profiles + recent projects, persisted as TOML in
//! the platform config dir (~/.config/blurb/settings.toml on Linux,
//! ~/Library/Application Support/blurb/ on macOS).

use harness_core::config::ProviderConfig;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Settings {
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    /// Index into `providers` of the default profile.
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
        let path = Self::path();
        let mut settings: Settings = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default();
        if settings.providers.is_empty() {
            settings.providers = ProviderConfig::default_profiles();
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

    pub fn remember_project(&mut self, path: PathBuf) {
        self.recent_projects.retain(|p| p != &path);
        self.recent_projects.insert(0, path);
        self.recent_projects.truncate(10);
    }
}
