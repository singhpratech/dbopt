use axum::response::sse::Event;
use futures::stream::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;

use crate::ollama::Message;

#[derive(Deserialize)]
pub struct Config {
    // The frontend's provider config object identifies the provider via `key`,
    // not `provider`, and the route handler overwrites this from the URL path
    // regardless — so it must default during deserialization, never be required.
    #[serde(default)]
    pub provider: String,       // "openai" | "openrouter" | "azure"
    pub model: String,
    pub api_key: String,
    pub base_url: Option<String>,
    pub deployment: Option<String>,
    pub api_version: Option<String>,
}

// Manual Debug that redacts the API key — never let a credential reach a log
// line or panic backtrace via {:?}.
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("api_key", &"<redacted>")
            .field("base_url", &self.base_url)
            .field("deployment", &self.deployment)
            .field("api_version", &self.api_version)
            .finish()
    }
}

#[derive(Debug, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    stream: bool,
    temperature: f32,
}

pub fn stream_chat(
    cfg: Config,
    messages: Vec<Message>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    async_stream::stream! {
        let (url, auth_header_name, auth_header_value) = match cfg.provider.as_str() {
            "openai" => (
                cfg.base_url.clone().unwrap_or_else(|| "https://api.openai.com/v1/chat/completions".into()),
                "Authorization".to_string(),
                format!("Bearer {}", cfg.api_key),
            ),
            "openrouter" => (
                cfg.base_url.clone().unwrap_or_else(|| "https://openrouter.ai/api/v1/chat/completions".into()),
                "Authorization".to_string(),
                format!("Bearer {}", cfg.api_key),
            ),
            "azure" => {
                let base = match &cfg.base_url {
                    Some(b) => b.clone(),
                    None => { yield Ok(Event::default().event("error").data("azure: base_url is required (https://<name>.openai.azure.com)")); return; }
                };
                let deployment = match &cfg.deployment {
                    Some(d) => d,
                    None => { yield Ok(Event::default().event("error").data("azure: deployment is required")); return; }
                };
                let ver = cfg.api_version.clone().unwrap_or_else(|| "2024-08-01-preview".into());
                let u = format!("{}/openai/deployments/{}/chat/completions?api-version={}", base.trim_end_matches('/'), deployment, ver);
                (u, "api-key".to_string(), cfg.api_key.clone())
            }
            other => {
                yield Ok(Event::default().event("error").data(format!("unknown openai-compat provider: {other}")));
                return;
            }
        };

        let body = ChatRequest { model: &cfg.model, messages: &messages, stream: true, temperature: 0.2 };
        let resp = match reqwest::Client::new()
            .post(&url)
            .header(&auth_header_name, &auth_header_value)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => { yield Ok(Event::default().event("error").data(e.to_string())); return; }
        };
        if !resp.status().is_success() {
            // Do NOT forward the upstream body to the browser — it can echo request
            // metadata. The status code is enough for the user to act on.
            let s = resp.status();
            yield Ok(Event::default().event("error").data(format!("provider returned HTTP {}", s.as_u16())));
            return;
        }

        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let bytes = match chunk { Ok(b) => b, Err(e) => { yield Ok(Event::default().event("error").data(e.to_string())); return; } };
            buf.extend_from_slice(&bytes);
            while let Some(nl) = buf.iter().position(|b| *b == b'\n') {
                let line = buf.drain(..=nl).collect::<Vec<u8>>();
                let s = match std::str::from_utf8(&line) { Ok(s) => s.trim().to_string(), Err(_) => continue };
                if !s.starts_with("data:") { continue; }
                let payload = s["data:".len()..].trim();
                if payload == "[DONE]" {
                    yield Ok(Event::default().event("done").data("end"));
                    return;
                }
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) {
                    if let Some(c) = v.pointer("/choices/0/delta/content").and_then(|x| x.as_str()) {
                        if !c.is_empty() { yield Ok(Event::default().data(c.to_string())); }
                    }
                }
            }
        }
        yield Ok(Event::default().event("done").data("end"));
    }
}
