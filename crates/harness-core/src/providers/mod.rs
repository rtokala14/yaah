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

/// Fetch the model catalog a provider connection offers. Anthropic and
/// OpenAI-compatible servers both expose a models listing; the returned ids
/// are ready to paste into `ModelConfig::id`. Blocking — run off-thread.
pub fn list_models(profile: &RunProfile) -> Result<Vec<String>, ProviderError> {
    let base = profile.resolved_base_url();
    if base.is_empty() {
        return Err(ProviderError::Config("base URL is required to list models".into()));
    }
    let key = profile.resolved_api_key();
    let (url, mut headers): (String, Vec<(String, String)>) = match profile.kind {
        ProviderKind::Anthropic => (
            format!("{base}/v1/models?limit=100"),
            vec![
                ("x-api-key".into(), key),
                ("anthropic-version".into(), "2023-06-01".into()),
            ],
        ),
        ProviderKind::OpenAi | ProviderKind::OpenAiCompat => (
            format!("{base}/models"),
            vec![("authorization".into(), format!("Bearer {key}"))],
        ),
    };
    headers.extend(profile.extra_headers.iter().cloned());
    let value = crate::http::get_json(&url, &headers)?;
    Ok(parse_model_ids(&value))
}

// ---------------------------------------------------------------------------
// Connection test

/// Outcome of a provider connection test, shaped for direct display.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeReport {
    pub ok: bool,
    /// One-line headline ("Working", "Auth failed", ...).
    pub headline: String,
    /// Detail plus, where we can infer one, a concrete next action.
    pub detail: String,
}

impl ProbeReport {
    fn ok(detail: impl Into<String>) -> Self {
        Self { ok: true, headline: "Working".into(), detail: detail.into() }
    }
    fn fail(headline: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { ok: false, headline: headline.into(), detail: detail.into() }
    }
}

/// Static problems in a profile, found without touching the network.
///
/// These are the misconfigurations that otherwise surface as confusing
/// runtime errors (a 404 from a doubled URL path, or an empty bearer token
/// because a literal key was pasted into the *variable name* field).
pub fn validate_profile(profile: &RunProfile) -> Vec<String> {
    let mut problems = Vec::new();

    if profile.model.trim().is_empty() {
        problems.push("No model id set.".into());
    }

    let base = profile.resolved_base_url();
    if base.is_empty() {
        problems.push("Base URL is required for OpenAI-compatible providers.".into());
    } else {
        if !base.starts_with("http://") && !base.starts_with("https://") {
            problems.push(format!("Base URL should start with http:// or https:// (got '{base}')."));
        }
        // The adapters append the endpoint path themselves, so a base URL that
        // already contains one yields '.../chat/completions/chat/completions'.
        for suffix in ["/chat/completions", "/v1/messages", "/completions", "/responses"] {
            if base.ends_with(suffix) {
                problems.push(format!(
                    "Base URL should not include the endpoint path '{suffix}' — the harness \
                     appends it. Use just the API root (e.g. 'https://host/v1')."
                ));
                break;
            }
        }
    }

    // A literal key in the env-var-name field silently resolves to nothing.
    let env_name = profile.api_key_env.trim();
    if !env_name.is_empty() && looks_like_secret(env_name) {
        problems.push(
            "The 'API key env var' field holds what looks like a key itself. That field takes \
             the *name* of an environment variable (e.g. OPENCODE_API_KEY); put the key in the \
             'API key' field instead."
                .into(),
        );
    }

    if profile.resolved_api_key().is_empty() {
        let hint = if env_name.is_empty() {
            "No API key set.".to_string()
        } else {
            format!("No API key: environment variable '{env_name}' is not set in this process.")
        };
        problems.push(hint);
    }

    problems
}

/// Heuristic: does this string look like a credential rather than a variable
/// name? Env var names are short, uppercase, `[A-Z0-9_]`; keys are long and
/// mixed-case, usually with a vendor prefix.
fn looks_like_secret(s: &str) -> bool {
    s.starts_with("sk-")
        || s.starts_with("pk-")
        || (s.len() > 40 && s.chars().any(|c| c.is_ascii_lowercase()))
}

/// Does this 403 body describe a *model* being refused rather than the
/// caller being unauthenticated? Observed shapes include Opencode's
/// `RegionError` ("only available hosted in China and requires explicit opt
/// in") and the usual plan/tier refusals.
fn is_model_entitlement(body: &str) -> bool {
    let b = body.to_ascii_lowercase();
    b.contains("regionerror")
        || b.contains("opt in")
        || b.contains("opt-in")
        || b.contains("not available")
        || b.contains("only available")
        || b.contains("does not have access to model")
        || b.contains("model_not_found")
        || b.contains("upgrade your plan")
}

/// Verify a provider+model actually works, end to end.
///
/// Deliberately exercises the *same* path a real run takes — build the
/// adapter, stream a 1-token completion — because that is the only way to
/// catch problems that a catalog `GET /models` would miss: a model id the
/// server doesn't recognize, tokens the key isn't entitled to, or a gateway
/// that only proxies chat. Blocking; run off the UI thread.
pub fn test_connection(profile: &RunProfile) -> ProbeReport {
    let problems = validate_profile(profile);
    if !problems.is_empty() {
        return ProbeReport::fail("Configuration incomplete", problems.join("\n"));
    }

    let provider = match build(profile) {
        Ok(p) => p,
        Err(e) => return ProbeReport::fail("Could not build provider", e.to_string()),
    };

    let req = ProviderRequest {
        system: "Reply with the single word: ok".into(),
        messages: vec![AgentMessage::User {
            content: vec![UserPart::Text { text: "ping".into() }],
        }],
        tools: vec![],
        // Reasoning models spend tokens before any visible text, so a tiny cap
        // would report a false failure. Keep it small but not starving.
        max_tokens: 512,
        effort: Some(Effort::Low),
        temperature: None,
    };

    let started = std::time::Instant::now();
    match provider.stream(&req, &mut |_| {}, &CancelToken::new()) {
        Ok(turn) => {
            let ms = started.elapsed().as_millis();
            let text: String = turn
                .content
                .iter()
                .filter_map(|p| match p {
                    AssistantPart::Text { text } => Some(text.trim()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");
            let reply = text.chars().take(60).collect::<String>();
            // Compat servers may omit usage entirely; zeros mean "not reported".
            let u = turn.usage;
            let usage = if u.input_tokens == 0 && u.output_tokens == 0 {
                String::new()
            } else {
                format!(", {} in / {} out tokens", u.input_tokens, u.output_tokens)
            };
            // A stream that closes without text still proves reachability +
            // auth, so report it as working rather than failing on emptiness.
            let detail = if reply.is_empty() {
                format!("{} responded in {ms} ms (no text in reply){usage}", profile.model)
            } else {
                format!("{} replied \"{reply}\" in {ms} ms{usage}", profile.model)
            };
            ProbeReport::ok(detail)
        }
        Err(e) => ProbeReport::fail(probe_headline(&e), explain_error(&e)),
    }
}

fn probe_headline(err: &ProviderError) -> String {
    match err {
        // A 403 is not necessarily a bad key: gateways also use it to refuse a
        // *specific model* (region locks, plan entitlement) while the same
        // credentials work fine elsewhere. Saying "Auth failed" there sends
        // people off rotating a perfectly good key.
        ProviderError::Api { status: 403, body } if is_model_entitlement(body) => {
            "Model unavailable".into()
        }
        ProviderError::Api { status: 401 | 403, .. } => "Auth failed".into(),
        ProviderError::Api { status: 404, .. } => "Not found".into(),
        ProviderError::Api { status: 429, .. } => "Rate limited".into(),
        ProviderError::Api { status, .. } if *status >= 500 => "Server error".into(),
        ProviderError::Api { .. } => "Request rejected".into(),
        ProviderError::Network(_) => "Could not connect".into(),
        ProviderError::Config(_) => "Configuration incomplete".into(),
        ProviderError::Cancelled => "Cancelled".into(),
        _ => "Failed".into(),
    }
}

/// Turn a raw provider error into something with a next action attached.
/// TLS interception in particular produces an error nobody can act on
/// without being told what it means.
pub fn explain_error(err: &ProviderError) -> String {
    let raw = err.to_string();
    let hint = match err {
        ProviderError::Network(msg) => {
            let m = msg.to_ascii_lowercase();
            if m.contains("unknownissuer") || m.contains("invalid peer certificate") {
                Some(
                    "The TLS certificate was signed by an issuer this build doesn't trust — \
                     typically a corporate TLS-inspecting proxy (Zscaler, Netskope, ...). \
                     The harness trusts the OS certificate store, so install the proxy's root \
                     CA there (it is usually already present on a managed machine).",
                )
            } else if m.contains("certificate") && m.contains("expired") {
                Some("The server's certificate is expired — check the system clock.")
            } else if m.contains("dns") || m.contains("resolve") {
                Some("The host could not be resolved — check the base URL for typos.")
            } else if m.contains("connection refused") {
                Some("Nothing is listening there — for a local server, confirm it is running.")
            } else if m.contains("timed out") || m.contains("timeout") {
                Some("The connection timed out — check the base URL, VPN, or proxy settings.")
            } else {
                None
            }
        }
        ProviderError::Api { status: 403, body } if is_model_entitlement(body) => Some(
            "The credentials are accepted, but this model is refused — it needs a plan, \
             region, or explicit opt-in your account doesn't have. Pick a different model \
             (use 'Fetch models' to see the catalog); the key itself is fine.",
        ),
        ProviderError::Api { status: 401 | 403, .. } => {
            Some("The server rejected the credentials — check the API key.")
        }
        ProviderError::Api { status: 404, .. } => Some(
            "The endpoint or model was not found — check the base URL is the API root \
             (without '/chat/completions') and that the model id exists.",
        ),
        _ => None,
    };
    match hint {
        Some(h) => format!("{raw}\n\n{h}"),
        None => raw,
    }
}

/// Both dialects wrap the catalog in `{"data": [{"id": ...}, ...]}`.
pub fn parse_model_ids(value: &serde_json::Value) -> Vec<String> {
    let mut ids: Vec<String> = value
        .get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("id").and_then(|v| v.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids.dedup();
    ids
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
    use super::{explain_error, parse_model_ids, sanitize_title, validate_profile};
    use crate::config::{ModelConfig, ProviderConfig, ProviderKind};
    use crate::types::ProviderError;

    /// Build the profile for an openai-compat provider under test.
    fn compat_profile(base_url: &str, api_key: &str, api_key_env: &str) -> crate::config::RunProfile {
        let mut p = ProviderConfig::new("Test", ProviderKind::OpenAiCompat);
        p.base_url = base_url.into();
        p.api_key = api_key.into();
        p.api_key_env = api_key_env.into();
        p.models = vec![ModelConfig::new("some-model")];
        p.normalize();
        p.resolve_active().unwrap()
    }

    #[test]
    fn validate_accepts_a_good_profile() {
        let profile = compat_profile("https://host/v1", "sk-live-key", "");
        assert!(validate_profile(&profile).is_empty());
    }

    #[test]
    fn validate_catches_endpoint_path_in_base_url() {
        // The real mistake: pasting the full chat-completions URL as base_url,
        // which the adapter then appends to again.
        let profile = compat_profile("https://opencode.ai/zen/go/v1/chat/completions", "sk-k", "");
        let problems = validate_profile(&profile);
        assert!(
            problems.iter().any(|p| p.contains("endpoint path")),
            "expected endpoint-path complaint, got {problems:?}"
        );
    }

    #[test]
    fn validate_catches_secret_pasted_into_env_var_field() {
        // api_key_env holds a *variable name*; a literal key there resolves to
        // nothing and the request goes out with an empty bearer token.
        let profile = compat_profile("https://host/v1", "", "sk-abcdefghijklmnopqrstuvwxyz0123456789");
        let problems = validate_profile(&profile);
        assert!(
            problems.iter().any(|p| p.contains("*name* of an environment variable")),
            "expected env-var-field complaint, got {problems:?}"
        );
        // And it must still report the resulting missing key.
        assert!(problems.iter().any(|p| p.contains("No API key")), "got {problems:?}");
    }

    #[test]
    fn validate_flags_missing_base_url_and_model() {
        let mut p = ProviderConfig::new("Test", ProviderKind::OpenAiCompat);
        p.models = vec![ModelConfig::new("")];
        p.normalize();
        let problems = validate_profile(&p.resolve_active().unwrap());
        assert!(problems.iter().any(|x| x.contains("Base URL is required")), "got {problems:?}");
        assert!(problems.iter().any(|x| x.contains("No model id")), "got {problems:?}");
    }

    #[test]
    fn validate_does_not_mistake_a_real_env_var_name_for_a_secret() {
        let profile = compat_profile("https://host/v1", "sk-key", "OPENCODE_API_KEY");
        let problems = validate_profile(&profile);
        assert!(
            !problems.iter().any(|p| p.contains("*name* of an environment variable")),
            "uppercase env var name must not trip the secret heuristic: {problems:?}"
        );
    }

    #[test]
    fn model_entitlement_403_is_not_reported_as_an_auth_failure() {
        // Verbatim body observed from Opencode's gateway for a region-locked
        // model, using a key that worked for every other model in the catalog.
        let err = ProviderError::Api {
            status: 403,
            body: r#"{"type":"error","error":{"type":"RegionError","message":"The latest version of this model is only available hosted in China and requires explicit opt in: https://opencode.ai/workspace/x"}}"#.into(),
        };
        assert_eq!(super::probe_headline(&err), "Model unavailable");
        let msg = explain_error(&err);
        assert!(msg.contains("key itself is fine"), "must not blame the key: {msg}");
        assert!(!msg.contains("check the API key"), "must not send user key-rotating: {msg}");

        // A genuine credential rejection still says so.
        let bad_key = ProviderError::Api {
            status: 403,
            body: r#"{"error":{"message":"invalid api key"}}"#.into(),
        };
        assert_eq!(super::probe_headline(&bad_key), "Auth failed");
        assert!(explain_error(&bad_key).contains("check the API key"));
    }

    #[test]
    fn explain_error_attaches_tls_interception_hint() {
        let err = ProviderError::Network(
            "tls connection init failed: invalid peer certificate: UnknownIssuer".into(),
        );
        let msg = explain_error(&err);
        assert!(msg.contains("UnknownIssuer"), "keeps the raw error: {msg}");
        assert!(msg.contains("proxy"), "explains interception: {msg}");
        // A plain network error gets no invented advice.
        let plain = explain_error(&ProviderError::Network("broken pipe".into()));
        assert_eq!(plain, "network: broken pipe");
    }

    #[test]
    fn parses_openai_and_anthropic_catalog_shapes() {
        let openai = serde_json::json!({"object": "list", "data": [
            {"id": "gpt-5.2", "object": "model"},
            {"id": "gpt-5.2-mini", "object": "model"},
            {"id": "gpt-5.2", "object": "model"} // dupes collapse
        ]});
        assert_eq!(parse_model_ids(&openai), vec!["gpt-5.2", "gpt-5.2-mini"]);

        let anthropic = serde_json::json!({"data": [
            {"id": "claude-opus-5", "display_name": "Claude Opus 5", "type": "model"}
        ], "has_more": false});
        assert_eq!(parse_model_ids(&anthropic), vec!["claude-opus-5"]);

        assert!(parse_model_ids(&serde_json::json!({"error": "nope"})).is_empty());
    }

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
