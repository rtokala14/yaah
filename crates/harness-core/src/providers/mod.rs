pub mod anthropic;
pub mod openai;

use crate::config::{ProviderKind, RunProfile};
use crate::types::*;
use std::sync::Arc;

/// Build a provider from a resolved (provider + model) run profile.
pub fn build(config: &RunProfile) -> Result<Arc<dyn Provider>, crate::types::ProviderError> {
    match config.kind {
        ProviderKind::Anthropic => Ok(Arc::new(anthropic::AnthropicProvider::new(config.clone())?)),
        ProviderKind::OpenAi | ProviderKind::OpenAiCompat => {
            Ok(Arc::new(openai::OpenAiProvider::new(config.clone())?))
        }
    }
}

/// One cheap low-effort call that names a session after its first task.
/// Blocking — run it off the UI thread.
pub fn generate_title(profile: &RunProfile, task: &str) -> Result<String, ProviderError> {
    let provider = build(profile)?;
    let task: String = task.chars().take(2000).collect();
    let req = ProviderRequest {
        system: "You name coding sessions. Reply with ONLY a title: 3-6 plain words, no quotes, no punctuation at the end.".into(),
        messages: vec![AgentMessage::User {
            content: vec![UserPart::Text {
                text: format!("Name the session for this task:\n\n{task}"),
            }],
        }],
        tools: vec![],
        max_tokens: 2000, // headroom for models that spend thinking tokens
        effort: Some(Effort::Low),
        temperature: None,
    };
    let turn = provider.stream(&req, &mut |_| {}, &CancelToken::new())?;
    let raw: String = turn
        .content
        .iter()
        .filter_map(|p| match p {
            AssistantPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    Ok(sanitize_title(&raw))
}

/// First non-empty line, stripped of quotes/markdown/trailing punctuation,
/// capped for the sidebar. Empty when the model produced nothing usable.
pub fn sanitize_title(raw: &str) -> String {
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let line = line.trim_matches(|c: char| c == '"' || c == '\'' || c == '`' || c == '#' || c == '*');
    let line = line.trim().trim_end_matches(['.', '!']);
    let mut title: String = line.chars().take(48).collect();
    if title.len() < line.chars().count() {
        title.push('…');
    }
    title.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::sanitize_title;

    #[test]
    fn sanitize_title_cleans_model_output() {
        assert_eq!(sanitize_title("\"Fix login retries\"\n"), "Fix login retries");
        assert_eq!(sanitize_title("**Refactor provider model.**"), "Refactor provider model");
        assert_eq!(sanitize_title("\n\n  Add git graph  \n"), "Add git graph");
        assert_eq!(sanitize_title(""), "");
        let long = "words ".repeat(20);
        assert!(sanitize_title(&long).chars().count() <= 49);
    }
}
