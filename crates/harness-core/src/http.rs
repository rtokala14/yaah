//! Blocking HTTP with retry for transient failures (408/429/5xx/529 and
//! network errors). Honors retry-after; exponential backoff with jitter
//! otherwise. Cancellation is checked between attempts and during backoff.

use crate::types::{CancelToken, ProviderError};
use std::io::Read;
use std::time::Duration;

pub struct HttpResponse {
    pub status: u16,
    pub reader: Box<dyn Read + Send>,
}

const RETRYABLE: &[u16] = &[408, 409, 429, 500, 502, 503, 504, 529];
const MAX_RETRIES: u32 = 4;

pub fn post_json_streaming(
    url: &str,
    headers: &[(String, String)],
    body: &serde_json::Value,
    cancel: &CancelToken,
) -> Result<HttpResponse, ProviderError> {
    let payload = serde_json::to_string(body)
        .map_err(|e| ProviderError::Config(format!("serialize body: {e}")))?;
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        // No overall timeout: SSE streams are long-lived. Read timeout guards
        // against a wedged connection between events.
        .timeout_read(Duration::from_secs(300))
        .build();

    let mut last_err: Option<ProviderError> = None;
    for attempt in 0..=MAX_RETRIES {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        if attempt > 0 {
            let base = 1000u64.saturating_mul(1 << (attempt - 1));
            let jitter = (pseudo_jitter() % 500) as u64;
            let delay = retry_after_ms(&last_err).unwrap_or(base.min(30_000) + jitter);
            sleep_cancellable(Duration::from_millis(delay), cancel)?;
        }

        let mut req = agent.post(url).set("content-type", "application/json");
        for (k, v) in headers {
            req = req.set(k, v);
        }
        match req.send_string(&payload) {
            Ok(res) => {
                return Ok(HttpResponse { status: res.status(), reader: Box::new(res.into_reader()) });
            }
            Err(ureq::Error::Status(status, res)) => {
                let body = res.into_string().unwrap_or_default();
                let retry_after = parse_retry_after(&body); // header lost by into_string; body hint only
                let err = ProviderError::Api { status, body: truncate(&body, 800) };
                if !RETRYABLE.contains(&status) {
                    return Err(err);
                }
                let _ = retry_after;
                last_err = Some(err);
            }
            Err(ureq::Error::Transport(t)) => {
                last_err = Some(ProviderError::Network(t.to_string()));
            }
        }
    }
    Err(last_err.unwrap_or(ProviderError::Network("exhausted retries".into())))
}

fn sleep_cancellable(total: Duration, cancel: &CancelToken) -> Result<(), ProviderError> {
    let step = Duration::from_millis(100);
    let mut elapsed = Duration::ZERO;
    while elapsed < total {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        std::thread::sleep(step.min(total - elapsed));
        elapsed += step;
    }
    Ok(())
}

fn retry_after_ms(err: &Option<ProviderError>) -> Option<u64> {
    // ureq 2.x drops headers once the body is consumed; providers also embed
    // retry hints in JSON error bodies — best-effort parse.
    if let Some(ProviderError::Api { body, .. }) = err {
        return parse_retry_after(body);
    }
    None
}

fn parse_retry_after(body: &str) -> Option<u64> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    v.get("error")?
        .get("retry_after")
        .and_then(|x| x.as_f64())
        .map(|s| (s * 1000.0) as u64)
}

fn pseudo_jitter() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

pub fn truncate(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        s.to_string()
    } else {
        let mut end = limit;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}
