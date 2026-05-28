use axum::response::sse::Event;
use futures::stream::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

fn ollama_url() -> String {
    std::env::var("OLLAMA_URL").unwrap_or_else(|_| "http://127.0.0.1:11434".into())
}

pub async fn list_models() -> anyhow::Result<serde_json::Value> {
    let url = format!("{}/api/tags", ollama_url());
    let resp = reqwest::get(&url).await?.json::<serde_json::Value>().await?;
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
        let resp = match reqwest::Client::new().post(&url).json(&body).send().await {
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
