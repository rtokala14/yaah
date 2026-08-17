pub mod anthropic;
pub mod openai;

use crate::config::{ProviderConfig, ProviderKind};
use crate::types::Provider;
use std::sync::Arc;

/// Build a provider from a user config profile.
pub fn build(config: &ProviderConfig) -> Result<Arc<dyn Provider>, crate::types::ProviderError> {
    match config.kind {
        ProviderKind::Anthropic => Ok(Arc::new(anthropic::AnthropicProvider::new(config.clone())?)),
        ProviderKind::OpenAi | ProviderKind::OpenAiCompat => {
            Ok(Arc::new(openai::OpenAiProvider::new(config.clone())?))
        }
    }
}
