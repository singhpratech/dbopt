use axum::response::sse::Event;
use futures::stream::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;

use crate::ollama::Message;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub model: String,
    pub api_key: String,
    pub base_url: Option<String>,
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Serialize)]
struct AnthropicReq<'a> {
    model: &'a str,
    max_tokens: u32,
    stream: bool,
    system: Option<&'a str>,
    messages: Vec<AntMessage<'a>>,
}

#[derive(Debug, Serialize)]
struct AntMessage<'a> {
    role: &'a str,
    content: &'a str,
}

pub fn stream_chat(
    cfg: Config,
    messages: Vec<Message>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    async_stream::stream! {
        let url = cfg.base_url.clone().unwrap_or_else(|| "https://api.anthropic.com/v1/messages".into());
        // Split out a system message if present
        let system = messages.iter().find(|m| m.role == "system").map(|m| m.content.clone());
        let convo: Vec<AntMessage> = messages.iter()
            .filter(|m| m.role != "system")
            .map(|m| AntMessage { role: if m.role == "assistant" { "assistant" } else { "user" }, content: &m.content })
            .collect();
        let body = AnthropicReq {
            model: &cfg.model,
            max_tokens: cfg.max_tokens.unwrap_or(2048),
            stream: true,
            system: system.as_deref(),
            messages: convo,
        };
        let resp = match reqwest::Client::new()
            .post(&url)
            .header("x-api-key", &cfg.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => { yield Ok(Event::default().event("error").data(e.to_string())); return; }
        };
        if !resp.status().is_success() {
            let s = resp.status();
            let txt = resp.text().await.unwrap_or_default();
            yield Ok(Event::default().event("error").data(format!("{}: {}", s, txt)));
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
                if payload.is_empty() { continue; }
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) {
                    let typ = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    if typ == "content_block_delta" {
                        if let Some(c) = v.pointer("/delta/text").and_then(|x| x.as_str()) {
                            if !c.is_empty() { yield Ok(Event::default().data(c.to_string())); }
                        }
                    } else if typ == "message_stop" {
                        yield Ok(Event::default().event("done").data("end"));
                        return;
                    } else if typ == "error" {
                        let m = v.pointer("/error/message").and_then(|x| x.as_str()).unwrap_or("anthropic error");
                        yield Ok(Event::default().event("error").data(m.to_string()));
                        return;
                    }
                }
            }
        }
        yield Ok(Event::default().event("done").data("end"));
    }
}
