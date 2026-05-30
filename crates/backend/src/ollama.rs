use axum::response::sse::Event;
use futures::stream::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::time::Duration;

/// Cap a single un-terminated stream line at 1 MiB. A provider that streams a
/// huge chunk without a newline would otherwise grow the buffer unbounded until
/// the always-on backend daemon OOMs — a crash that takes the whole tool down.
const MAX_LINE_SIZE: usize = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

fn ollama_url() -> String {
    std::env::var("OLLAMA_URL").unwrap_or_else(|_| "http://127.0.0.1:11434".into())
}

/// HTTP client with a timeout so a hung Ollama can't pin a request open forever.
/// Streaming chats can legitimately run long, so callers pass a generous value.
fn http_client(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

pub async fn list_models() -> anyhow::Result<serde_json::Value> {
    let url = format!("{}/api/tags", ollama_url());
    let resp = http_client(Duration::from_secs(15))
        .get(&url)
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;
    Ok(resp)
}

pub fn stream_chat(
    model: String,
    messages: Vec<Message>,
    options: Option<serde_json::Value>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    async_stream::stream! {
        let url = format!("{}/api/chat", ollama_url());
        let body = serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": true,
            "options": options.unwrap_or(serde_json::json!({})),
        });
        let resp = match http_client(Duration::from_secs(120)).post(&url).json(&body).send().await {
            Ok(r) => r,
            Err(e) => {
                yield Ok(Event::default().event("error").data(e.to_string()));
                return;
            }
        };
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let bytes = match chunk {
                Ok(b) => b,
                Err(e) => { yield Ok(Event::default().event("error").data(e.to_string())); return; }
            };
            buf.extend_from_slice(&bytes);
            if buf.len() > MAX_LINE_SIZE {
                yield Ok(Event::default().event("error").data("provider response line exceeded 1MB limit"));
                return;
            }
            while let Some(nl) = buf.iter().position(|b| *b == b'\n') {
                let line = buf.drain(..=nl).collect::<Vec<u8>>();
                let s = match std::str::from_utf8(&line) { Ok(s) => s.trim().to_string(), Err(_) => continue };
                if s.is_empty() { continue; }
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                    if let Some(c) = v.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_str()) {
                        if !c.is_empty() {
                            yield Ok(Event::default().data(c.to_string()));
                        }
                    }
                    if v.get("done").and_then(|d| d.as_bool()).unwrap_or(false) {
                        yield Ok(Event::default().event("done").data("end"));
                        return;
                    }
                }
            }
        }
    }
}
