//! Provider discovery: validate an API key and list a provider's models.
//!
//! These run server-side (proxied through the backend) for two reasons:
//!   1. browsers would hit CORS calling provider APIs directly, and
//!   2. it keeps the key handling identical to the chat path.
//!
//! Supported: openrouter, openai, anthropic. Azure/Bedrock/Ollama/web-llm
//! don't expose a uniform "list models / check key" HTTP shape, so they return
//! an explicit "not supported" error the UI can hide gracefully.

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

fn key_of(cfg: &Value) -> String {
    cfg.get("api_key").and_then(|v| v.as_str()).unwrap_or("").to_string()
}

/// List the models a provider offers. For OpenRouter the catalog is public, so
/// no key is required; OpenAI/Anthropic need a valid key.
pub async fn list_models(provider: &str, cfg: &Value) -> Result<Vec<ModelInfo>, String> {
    let client = reqwest::Client::new();
    match provider {
        "openrouter" => {
            let resp = client
                .get("https://openrouter.ai/api/v1/models")
                .send()
                .await
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("{}: {}", resp.status(), resp.text().await.unwrap_or_default()));
            }
            let v: Value = resp.json().await.map_err(|e| e.to_string())?;
            let arr = v.get("data").and_then(|d| d.as_array()).ok_or("unexpected response (no data[])")?;
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
            Ok(out)
        }
        "openai" => {
            let key = key_of(cfg);
            if key.is_empty() {
                return Err("API key not set".into());
            }
            let resp = client
                .get("https://api.openai.com/v1/models")
                .bearer_auth(&key)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("{}: {}", resp.status(), resp.text().await.unwrap_or_default()));
            }
            let v: Value = resp.json().await.map_err(|e| e.to_string())?;
            let arr = v.get("data").and_then(|d| d.as_array()).ok_or("unexpected response (no data[])")?;
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
                return Err("API key not set".into());
            }
            let resp = client
                .get("https://api.anthropic.com/v1/models")
                .header("x-api-key", &key)
                .header("anthropic-version", "2023-06-01")
                .send()
                .await
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("{}: {}", resp.status(), resp.text().await.unwrap_or_default()));
            }
            let v: Value = resp.json().await.map_err(|e| e.to_string())?;
            let arr = v.get("data").and_then(|d| d.as_array()).ok_or("unexpected response (no data[])")?;
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
        other => Err(format!("model listing not supported for '{other}'")),
    }
}

/// Validate a key by hitting a cheap authenticated endpoint. For OpenRouter we
/// also fold in the remaining credit balance.
pub async fn test_key(provider: &str, cfg: &Value) -> Result<TestResult, String> {
    let client = reqwest::Client::new();
    let key = key_of(cfg);
    match provider {
        "openrouter" => {
            if key.is_empty() {
                return Err("API key not set".into());
            }
            let resp = client
                .get("https://openrouter.ai/api/v1/auth/key")
                .bearer_auth(&key)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Ok(TestResult { ok: false, detail: format!("invalid key ({})", resp.status()), credits_remaining: None });
            }
            // Best-effort credits lookup; the key is already proven valid above.
            let mut remaining = None;
            let mut detail = "key valid".to_string();
            if let Ok(cr) = client.get("https://openrouter.ai/api/v1/credits").bearer_auth(&key).send().await {
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
            if key.is_empty() {
                return Err("API key not set".into());
            }
            let resp = client
                .get("https://api.openai.com/v1/models")
                .bearer_auth(&key)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let ok = resp.status().is_success();
            if !ok {
                return Ok(TestResult { ok: false, detail: format!("invalid key ({})", resp.status()), credits_remaining: None });
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
            if key.is_empty() {
                return Err("API key not set".into());
            }
            let resp = client
                .get("https://api.anthropic.com/v1/models")
                .header("x-api-key", &key)
                .header("anthropic-version", "2023-06-01")
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let ok = resp.status().is_success();
            Ok(TestResult {
                ok,
                detail: if ok { "key valid".into() } else { format!("invalid key ({})", resp.status()) },
                credits_remaining: None,
            })
        }
        other => Err(format!("key test not supported for '{other}'")),
    }
}
