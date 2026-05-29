//! Provider discovery: validate an API key and list a provider's models.
//!
//! These run server-side (proxied through the backend) for two reasons:
//!   1. browsers would hit CORS calling provider APIs directly, and
//!   2. it keeps the key handling identical to the chat path.
//!
//! Supported: openrouter, openai, anthropic. Azure/Bedrock/Ollama/web-llm
//! don't expose a uniform "list models / check key" HTTP shape, so they return
//! a BadRequest the UI can hide gracefully.
//!
//! Error discipline: we NEVER forward an upstream response body to the client
//! (it can echo request metadata / keys); only the status code. And we split
//! client mistakes (BadRequest -> HTTP 400) from upstream/network failures
//! (Upstream -> HTTP 502) so callers can tell "fix your config" from "try later".

use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: Option<String>,
    pub context: Option<u64>,
    /// USD per 1M prompt tokens (when the provider publishes pricing).
    pub price_in: Option<f64>,
    /// USD per 1M completion tokens.
    pub price_out: Option<f64>,
    pub free: bool,
}

#[derive(Serialize)]
pub struct TestResult {
    pub ok: bool,
    pub detail: String,
    pub credits_remaining: Option<f64>,
}

/// Distinguishes a client-side mistake (-> 400) from an upstream/network
/// failure (-> 502). Messages are always safe to surface (status codes /
/// fixed strings) — never an upstream response body.
pub enum DiscoverError {
    BadRequest(String),
    Upstream(String),
}

impl std::fmt::Display for DiscoverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiscoverError::BadRequest(m) | DiscoverError::Upstream(m) => f.write_str(m),
        }
    }
}

fn key_of(cfg: &Value) -> String {
    cfg.get("api_key").and_then(|v| v.as_str()).unwrap_or("").to_string()
}

/// Shared client with a sane timeout so a hung provider can't pin a request open.
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(25))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// List the models a provider offers. For OpenRouter the catalog is public, so
/// no key is required; OpenAI/Anthropic need a valid key.
pub async fn list_models(provider: &str, cfg: &Value) -> Result<Vec<ModelInfo>, DiscoverError> {
    let c = client();
    match provider {
        "openrouter" => {
            let resp = c
                .get("https://openrouter.ai/api/v1/models")
                .send()
                .await
                .map_err(|e| DiscoverError::Upstream(format!("openrouter unreachable: {}", net_msg(&e))))?;
            if !resp.status().is_success() {
                return Err(DiscoverError::Upstream(format!("openrouter returned HTTP {}", resp.status().as_u16())));
            }
            let v: Value = resp.json().await.map_err(|_| DiscoverError::Upstream("openrouter sent a malformed response".into()))?;
            Ok(parse_openrouter_models(&v))
        }
        "openai" => {
            let key = key_of(cfg);
            if key.is_empty() {
                return Err(DiscoverError::BadRequest("API key not set".into()));
            }
            let resp = c
                .get("https://api.openai.com/v1/models")
                .bearer_auth(&key)
                .send()
                .await
                .map_err(|e| DiscoverError::Upstream(format!("openai unreachable: {}", net_msg(&e))))?;
            if !resp.status().is_success() {
                return Err(status_error("openai", resp.status()));
            }
            let v: Value = resp.json().await.map_err(|_| DiscoverError::Upstream("openai sent a malformed response".into()))?;
            let arr = v.get("data").and_then(|d| d.as_array()).cloned().unwrap_or_default();
            let mut out: Vec<ModelInfo> = arr
                .iter()
                .filter_map(|m| {
                    let id = m.get("id")?.as_str()?.to_string();
                    Some(ModelInfo { id, name: None, context: None, price_in: None, price_out: None, free: false })
                })
                .collect();
            out.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(out)
        }
        "anthropic" => {
            let key = key_of(cfg);
            if key.is_empty() {
                return Err(DiscoverError::BadRequest("API key not set".into()));
            }
            let resp = c
                .get("https://api.anthropic.com/v1/models")
                .header("x-api-key", &key)
                .header("anthropic-version", "2023-06-01")
                .send()
                .await
                .map_err(|e| DiscoverError::Upstream(format!("anthropic unreachable: {}", net_msg(&e))))?;
            if !resp.status().is_success() {
                return Err(status_error("anthropic", resp.status()));
            }
            let v: Value = resp.json().await.map_err(|_| DiscoverError::Upstream("anthropic sent a malformed response".into()))?;
            let arr = v.get("data").and_then(|d| d.as_array()).cloned().unwrap_or_default();
            let out = arr
                .iter()
                .filter_map(|m| {
                    let id = m.get("id")?.as_str()?.to_string();
                    let name = m.get("display_name").and_then(|x| x.as_str()).map(str::to_string);
                    Some(ModelInfo { id, name, context: None, price_in: None, price_out: None, free: false })
                })
                .collect();
            Ok(out)
        }
        other => Err(DiscoverError::BadRequest(format!("model listing not supported for '{other}'"))),
    }
}

/// Validate a key by hitting a cheap authenticated endpoint. For OpenRouter we
/// also fold in the remaining credit balance. A non-200 from the provider for a
/// key check is a *test result* (ok:false), not an error — so the caller gets a
/// clean 200 with `ok:false`. Missing key / unsupported provider are 400s;
/// network failures are 502s.
pub async fn test_key(provider: &str, cfg: &Value) -> Result<TestResult, DiscoverError> {
    let c = client();
    let key = key_of(cfg);
    if matches!(provider, "openrouter" | "openai" | "anthropic") && key.is_empty() {
        return Err(DiscoverError::BadRequest("API key not set".into()));
    }
    match provider {
        "openrouter" => {
            let resp = c
                .get("https://openrouter.ai/api/v1/auth/key")
                .bearer_auth(&key)
                .send()
                .await
                .map_err(|e| DiscoverError::Upstream(format!("openrouter unreachable: {}", net_msg(&e))))?;
            if !resp.status().is_success() {
                return Ok(TestResult { ok: false, detail: format!("invalid key (HTTP {})", resp.status().as_u16()), credits_remaining: None });
            }
            // Best-effort credits lookup; the key is already proven valid above.
            let mut remaining = None;
            let mut detail = "key valid".to_string();
            if let Ok(cr) = c.get("https://openrouter.ai/api/v1/credits").bearer_auth(&key).send().await {
                if let Ok(cv) = cr.json::<Value>().await {
                    let total = cv.pointer("/data/total_credits").and_then(|x| x.as_f64());
                    let used = cv.pointer("/data/total_usage").and_then(|x| x.as_f64());
                    if let (Some(t), Some(u)) = (total, used) {
                        remaining = Some(t - u);
                        detail = format!("key valid · ${:.2} of ${:.0} remaining", t - u, t);
                    }
                }
            }
            Ok(TestResult { ok: true, detail, credits_remaining: remaining })
        }
        "openai" => {
            let resp = c
                .get("https://api.openai.com/v1/models")
                .bearer_auth(&key)
                .send()
                .await
                .map_err(|e| DiscoverError::Upstream(format!("openai unreachable: {}", net_msg(&e))))?;
            if !resp.status().is_success() {
                return Ok(TestResult { ok: false, detail: format!("invalid key (HTTP {})", resp.status().as_u16()), credits_remaining: None });
            }
            let n = resp
                .json::<Value>()
                .await
                .ok()
                .and_then(|v| v.get("data").and_then(|d| d.as_array()).map(|a| a.len()))
                .unwrap_or(0);
            Ok(TestResult { ok: true, detail: format!("key valid · {n} models"), credits_remaining: None })
        }
        "anthropic" => {
            let resp = c
                .get("https://api.anthropic.com/v1/models")
                .header("x-api-key", &key)
                .header("anthropic-version", "2023-06-01")
                .send()
                .await
                .map_err(|e| DiscoverError::Upstream(format!("anthropic unreachable: {}", net_msg(&e))))?;
            if resp.status().is_success() {
                Ok(TestResult { ok: true, detail: "key valid".into(), credits_remaining: None })
            } else {
                Ok(TestResult { ok: false, detail: format!("invalid key (HTTP {})", resp.status().as_u16()), credits_remaining: None })
            }
        }
        other => Err(DiscoverError::BadRequest(format!("key test not supported for '{other}'"))),
    }
}

/// Map a non-2xx provider status to a client (auth) vs upstream error, with a
/// message that never includes the response body.
fn status_error(provider: &str, status: reqwest::StatusCode) -> DiscoverError {
    let code = status.as_u16();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        DiscoverError::BadRequest(format!("{provider} rejected the API key (HTTP {code})"))
    } else {
        DiscoverError::Upstream(format!("{provider} returned HTTP {code}"))
    }
}

/// reqwest errors can carry a URL (with query) — keep only the coarse reason.
fn net_msg(e: &reqwest::Error) -> &'static str {
    if e.is_timeout() {
        "request timed out"
    } else if e.is_connect() {
        "connection failed"
    } else {
        "network error"
    }
}

fn parse_openrouter_models(v: &Value) -> Vec<ModelInfo> {
    let arr = match v.get("data").and_then(|d| d.as_array()) {
        Some(a) => a,
        None => return Vec::new(),
    };
    let mut out = Vec::with_capacity(arr.len());
    for m in arr {
        let id = match m.get("id").and_then(|x| x.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };
        let name = m.get("name").and_then(|x| x.as_str()).map(str::to_string);
        let context = m.get("context_length").and_then(|x| x.as_u64());
        let price = |k: &str| {
            m.pointer(&format!("/pricing/{k}"))
                .and_then(|x| x.as_str())
                .and_then(|s| s.parse::<f64>().ok())
                .map(|f| f * 1_000_000.0)
        };
        let price_in = price("prompt");
        let price_out = price("completion");
        let free = price_in.unwrap_or(0.0) == 0.0 && price_out.unwrap_or(0.0) == 0.0;
        out.push(ModelInfo { id, name, context, price_in, price_out, free });
    }
    out
}
