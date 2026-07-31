//! 客户端集成测试。

use anthropic_rust_sdk::{Anthropic, MessageContent, MessageCreateParams, MessageParam, Role};
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn messages_create_sends_expected_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-key"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "msg_01",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "Hi"}],
            "model": "claude-opus-4-6",
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {"input_tokens": 10, "output_tokens": 5}
        })))
        .mount(&server)
        .await;

    let client = Anthropic::with_options(anthropic_rust_sdk::ClientOptions {
        api_key: Some("test-key".into()),
        base_url: Some(server.uri()),
        ..Default::default()
    })
    .unwrap();

    let params = MessageCreateParams::new(
        "claude-opus-4-6",
        1024,
        vec![MessageParam {
            role: Role::User,
            content: MessageContent::Text("Hello".into()),
        }],
    );

    let result = client.messages().create(params).await.unwrap();
    match result {
        anthropic_rust_sdk::MessageCreateResult::Message(m) => {
            assert_eq!(m.content[0].text(), Some("Hi"));
        }
        _ => panic!("expected non-streaming message"),
    }
}

#[tokio::test]
async fn messages_create_supports_bearer_only_authentication() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("authorization", "Bearer test-token"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "msg_01",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "Hi"}],
            "model": "claude-opus-4-6",
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {"input_tokens": 10, "output_tokens": 5}
        })))
        .mount(&server)
        .await;

    let client = Anthropic::with_options(anthropic_rust_sdk::ClientOptions {
        api_key: None,
        auth_token: Some("test-token".into()),
        base_url: Some(server.uri()),
        ..Default::default()
    })
    .unwrap();

    let params = MessageCreateParams::new(
        "claude-opus-4-6",
        1024,
        vec![MessageParam {
            role: Role::User,
            content: MessageContent::Text("Hello".into()),
        }],
    );

    let result = client.messages().create(params).await.unwrap();
    assert!(matches!(
        result,
        anthropic_rust_sdk::MessageCreateResult::Message(_)
    ));

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert!(!requests[0].headers.contains_key("x-api-key"));
}

#[tokio::test]
async fn maps_401_to_authentication_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": {"type": "authentication_error", "message": "invalid x-api-key"}
        })))
        .mount(&server)
        .await;

    let client = Anthropic::with_options(anthropic_rust_sdk::ClientOptions {
        api_key: Some("bad-key".into()),
        base_url: Some(server.uri()),
        max_retries: Some(0),
        ..Default::default()
    })
    .unwrap();

    let params = MessageCreateParams::new(
        "claude-opus-4-6",
        1024,
        vec![MessageParam {
            role: Role::User,
            content: MessageContent::Text("Hello".into()),
        }],
    );

    let result = client.messages().create(params).await;
    assert!(matches!(
        result,
        Err(anthropic_rust_sdk::Error::Authentication(_))
    ));
}

#[tokio::test]
async fn count_tokens_parses_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages/count_tokens"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "input_tokens": 42
        })))
        .mount(&server)
        .await;

    let client = Anthropic::with_options(anthropic_rust_sdk::ClientOptions {
        api_key: Some("test-key".into()),
        base_url: Some(server.uri()),
        ..Default::default()
    })
    .unwrap();

    let count = client
        .messages()
        .count_tokens(anthropic_rust_sdk::MessageCountTokensParams {
            model: "claude-opus-4-6".into(),
            messages: vec![MessageParam {
                role: Role::User,
                content: MessageContent::Text("Hello".into()),
            }],
            system: None,
            tools: None,
            user_profile_id: None,
            extra: serde_json::Value::Null,
        })
        .await
        .unwrap();

    assert_eq!(count.input_tokens, 42);
}

#[tokio::test]
async fn create_puts_user_profile_id_in_header_not_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("anthropic-user-profile-id", "profile-123"))
        .and(body_json(serde_json::json!({
            "model": "claude-opus-4-6",
            "max_tokens": 1024,
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": false
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "msg_01",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "Hi"}],
            "model": "claude-opus-4-6",
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {"input_tokens": 10, "output_tokens": 5}
        })))
        .mount(&server)
        .await;

    let client = Anthropic::with_options(anthropic_rust_sdk::ClientOptions {
        api_key: Some("test-key".into()),
        base_url: Some(server.uri()),
        ..Default::default()
    })
    .unwrap();

    let mut params = MessageCreateParams::new(
        "claude-opus-4-6",
        1024,
        vec![MessageParam {
            role: Role::User,
            content: MessageContent::Text("Hello".into()),
        }],
    );
    params.user_profile_id = Some("profile-123".into());

    let result = client.messages().create(params).await.unwrap();
    match result {
        anthropic_rust_sdk::MessageCreateResult::Message(m) => {
            assert_eq!(m.content[0].text(), Some("Hi"));
        }
        _ => panic!("expected non-streaming message"),
    }
}
