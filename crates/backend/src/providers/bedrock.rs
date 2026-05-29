use axum::response::sse::Event;
use futures::stream::Stream;
use serde::Deserialize;
use std::convert::Infallible;

use crate::ollama::Message;

#[derive(Deserialize)]
pub struct Config {
    pub model: String,
    pub region: String,
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<String>,
    pub session_token: Option<String>,
}

// Manual Debug that redacts the AWS credentials.
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redact = |o: &Option<String>| if o.is_some() { "<redacted>" } else { "None" };
        f.debug_struct("Config")
            .field("model", &self.model)
            .field("region", &self.region)
            .field("access_key_id", &redact(&self.access_key_id))
            .field("secret_access_key", &redact(&self.secret_access_key))
            .field("session_token", &redact(&self.session_token))
            .finish()
    }
}

#[cfg(not(feature = "bedrock"))]
pub fn stream_chat(_cfg: Config, _messages: Vec<Message>) -> impl Stream<Item = Result<Event, Infallible>> {
    async_stream::stream! {
        yield Ok(Event::default().event("error").data(
            "AWS Bedrock provider is not compiled in. Rebuild the backend with `cargo build -p backend --features bedrock` to enable it. (Adds aws-sdk-bedrockruntime — multi-megabyte deps.)"
        ));
    }
}

#[cfg(feature = "bedrock")]
pub fn stream_chat(cfg: Config, messages: Vec<Message>) -> impl Stream<Item = Result<Event, Infallible>> {
    async_stream::stream! {
        use aws_sdk_bedrockruntime::{Client as BedrockClient, types::{Message as AwsMessage, ContentBlock, ConversationRole, SystemContentBlock}};
        use aws_config::BehaviorVersion;
        use futures::StreamExt;

        let mut loader = aws_config::defaults(BehaviorVersion::latest()).region(aws_sdk_bedrockruntime::config::Region::new(cfg.region.clone()));
        if let (Some(k), Some(s)) = (cfg.access_key_id.as_deref(), cfg.secret_access_key.as_deref()) {
            let creds = aws_credential_types::Credentials::new(k, s, cfg.session_token.clone(), None, "sqlopt");
            loader = loader.credentials_provider(creds);
        }
        let conf = loader.load().await;
        let client = BedrockClient::new(&conf);

        let system_msgs: Vec<SystemContentBlock> = messages.iter().filter(|m| m.role == "system")
            .map(|m| SystemContentBlock::Text(m.content.clone())).collect();
        let convo: Vec<AwsMessage> = messages.iter().filter(|m| m.role != "system").filter_map(|m| {
            let role = if m.role == "assistant" { ConversationRole::Assistant } else { ConversationRole::User };
            AwsMessage::builder().role(role).content(ContentBlock::Text(m.content.clone())).build().ok()
        }).collect();

        let mut request = client.converse_stream().model_id(&cfg.model);
        for s in system_msgs { request = request.system(s); }
        for m in convo { request = request.messages(m); }
        let mut out = match request.send().await {
            Ok(r) => r,
            Err(e) => { yield Ok(Event::default().event("error").data(format!("{e}"))); return; }
        };

        while let Some(event) = out.stream.recv().await.transpose() {
            let event = match event {
                Ok(e) => e,
                Err(e) => { yield Ok(Event::default().event("error").data(format!("{e}"))); return; }
            };
            use aws_sdk_bedrockruntime::types::ConverseStreamOutput::*;
            match event {
                ContentBlockDelta(d) => {
                    if let Some(delta) = d.delta {
                        if let aws_sdk_bedrockruntime::types::ContentBlockDelta::Text(t) = delta {
                            yield Ok(Event::default().data(t));
                        }
                    }
                }
                MessageStop(_) => { yield Ok(Event::default().event("done").data("end")); return; }
                _ => {}
            }
        }
        yield Ok(Event::default().event("done").data("end"));
    }
}
