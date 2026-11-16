use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use futures::StreamExt;
use goose::agents::{
    AgentEvent, GoosePlatform, SessionConfig, StateMachineServices, StateMachineServicesConfig,
};
use goose::config::permission::PermissionManager;
use goose::config::GooseMode;
use goose::conversation::message::{Message, MessageContent};
use goose::providers::base::{MessageStream, Provider, ProviderDef, ProviderMetadata};
use goose::session::session_manager::SessionType;
use goose::session::SessionManager;
use goose_providers::errors::ProviderError;
use goose_providers::formats::anthropic;
use goose_providers::model::ModelConfig;
use rmcp::model::{Role, Tool};

const THINKING_STREAM: &[&str] = &[
    r#"data: {"type":"message_start","message":{"id":"msg_think","role":"assistant","content":[],"model":"claude-opus-4-6","usage":{"input_tokens":10,"output_tokens":0}}}"#,
    r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
    r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me "}}"#,
    r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"think "}}"#,
    r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"carefully."}}"#,
    r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig_a"}}"#,
    r#"data: {"type":"content_block_stop","index":0}"#,
    r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"thinking","thinking":""}}"#,
    r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"signature_delta","signature":"sig_omitted"}}"#,
    r#"data: {"type":"content_block_stop","index":1}"#,
    r#"data: {"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}}"#,
    r#"data: {"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"Done "}}"#,
    r#"data: {"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"thinking."}}"#,
    r#"data: {"type":"content_block_stop","index":2}"#,
    r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":20}}"#,
    r#"data: {"type":"message_stop"}"#,
];

struct AnthropicSseProvider;

impl goose::providers::base::ProviderDescriptor for AnthropicSseProvider {
    fn metadata() -> ProviderMetadata {
        ProviderMetadata {
            name: "anthropic-sse-mock".to_string(),
            display_name: "Anthropic SSE Mock".to_string(),
            description: "Replays a recorded Anthropic thinking stream".to_string(),
            default_model: "mock-model".to_string(),
            known_models: vec![],
            model_doc_link: "".to_string(),
            config_keys: vec![],
            setup_steps: vec![],
            setup: None,
            deprecated: None,
        }
    }
}

impl ProviderDef for AnthropicSseProvider {
    type Provider = Self;

    fn from_env(
        _extensions: Vec<goose::config::ExtensionConfig>,
        _tls_config: Option<goose::providers::api_client::TlsConfig>,
    ) -> futures::future::BoxFuture<'static, anyhow::Result<Self>> {
        unimplemented!()
    }
}

#[async_trait]
impl Provider for AnthropicSseProvider {
    async fn stream(
        &self,
        _model_config: &ModelConfig,
        _system_prompt: &str,
        _messages: &[Message],
        _tools: &[Tool],
    ) -> Result<MessageStream, ProviderError> {
        let lines = futures::stream::iter(THINKING_STREAM.iter().map(|line| Ok(line.to_string())));
        Ok(Box::pin(
            anthropic::response_to_streaming_message(lines)
                .map(|item| item.map_err(ProviderError::from_stream_error)),
        ))
    }

    fn get_name(&self) -> &str {
        "anthropic-sse-mock"
    }
}

fn thinking_blocks(message: &Message) -> Vec<(String, String)> {
    message
        .content
        .iter()
        .filter_map(|content| match content {
            MessageContent::Thinking(t) => Some((t.thinking.clone(), t.signature.clone())),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn streamed_thinking_deltas_are_stored_as_one_block_per_provider_block() -> Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let session_manager = Arc::new(SessionManager::new(temp_dir.path().to_path_buf()));
    let services = StateMachineServices::with_config(StateMachineServicesConfig::new(
        session_manager.clone(),
        PermissionManager::instance(),
        None,
        true,
        GoosePlatform::GooseCli,
    ));
    let session = session_manager
        .create_session(
            PathBuf::default(),
            "thinking-stream-storage".to_string(),
            SessionType::Hidden,
            GooseMode::default(),
        )
        .await?;
    services
        .update_provider(
            Arc::new(AnthropicSseProvider),
            ModelConfig::new("mock-model"),
            &session.id,
        )
        .await?;

    let reply = services
        .reply(
            Message::user().with_text("Think, then answer"),
            SessionConfig {
                id: session.id.clone(),
                schedule_id: None,
                max_turns: Some(1),
            },
            None,
        )
        .await?;
    tokio::pin!(reply);

    let mut live_thinking = Vec::new();
    while let Some(event) = reply.next().await {
        if let AgentEvent::Message(message) = event? {
            live_thinking.extend(thinking_blocks(&message));
        }
    }
    assert_eq!(
        live_thinking,
        vec![
            ("Let me ".to_string(), String::new()),
            ("think ".to_string(), String::new()),
            ("carefully.".to_string(), String::new()),
            (String::new(), "sig_a".to_string()),
            (String::new(), "sig_omitted".to_string()),
        ],
        "each thinking delta must reach the client as it arrives"
    );

    let stored: Vec<Message> = session_manager
        .get_session(&session.id, true)
        .await?
        .conversation
        .expect("session should have a conversation")
        .messages()
        .iter()
        .filter(|message| message.role == Role::Assistant)
        .cloned()
        .collect();
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert_eq!(
        thinking_blocks(&stored[0]),
        vec![
            ("Let me think carefully.".to_string(), "sig_a".to_string()),
            (String::new(), "sig_omitted".to_string()),
        ]
    );
    assert_eq!(stored[0].as_concat_text(), "Done thinking.");

    let replay = anthropic::format_messages(&stored);
    assert_eq!(
        replay[0]["content"],
        serde_json::json!([
            {"type": "thinking", "thinking": "Let me think carefully.", "signature": "sig_a"},
            {"type": "thinking", "thinking": "", "signature": "sig_omitted"},
            {"type": "text", "text": "Done thinking."},
        ])
    );
    Ok(())
}
