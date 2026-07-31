//! 新增 beta 资源（dreams / tunnels / tunnel certificates）的集成测试。
//!
//! 覆盖核心 URL 拼接、`anthropic-beta` 头注入与分页解析。

use anthropic_rust_sdk::{Anthropic, ClientOptions};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn build_client(server: &MockServer) -> Anthropic {
    Anthropic::with_options(ClientOptions {
        api_key: Some("test-key".into()),
        base_url: Some(server.uri()),
        ..Default::default()
    })
    .unwrap()
}

#[tokio::test]
async fn dreams_create_hits_endpoint_with_beta_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/dreams"))
        .and(header("x-api-key", "test-key"))
        .and(header("anthropic-beta", "dreaming-2026-04-21"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "dream_01",
            "type": "dream",
            "status": "queued"
        })))
        .mount(&server)
        .await;

    let client = build_client(&server);
    let dream = client
        .beta()
        .with_beta_headers(vec!["dreaming-2026-04-21".into()])
        .dreams()
        .create(&serde_json::json!({"prompt": "hello"}))
        .await
        .unwrap();

    assert_eq!(dream.id, "dream_01");
    assert_eq!(dream.extra["status"], "queued");
}

#[tokio::test]
async fn dreams_list_parses_page_cursor() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/dreams"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{"id": "dream_01", "type": "dream"}],
            "has_more": false
        })))
        .mount(&server)
        .await;

    let client = build_client(&server);
    let page = client.beta().dreams().list().await.unwrap();

    assert_eq!(page.data.len(), 1);
    assert_eq!(page.data[0].id, "dream_01");
    assert!(!page.has_more);
}

#[tokio::test]
async fn tunnels_create_hits_endpoint_with_beta_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/tunnels"))
        .and(header("anthropic-beta", "mcp-tunnels-2026-06-22"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "tnl_01",
            "type": "tunnel",
            "domain": "example.tunnel.anthropic.com"
        })))
        .mount(&server)
        .await;

    let client = build_client(&server);
    let tunnel = client
        .beta()
        .with_beta_headers(vec!["mcp-tunnels-2026-06-22".into()])
        .tunnels()
        .create(&serde_json::json!({"display_name": "dev"}))
        .await
        .unwrap();

    assert_eq!(tunnel.id, "tnl_01");
    assert_eq!(tunnel.extra["domain"], "example.tunnel.anthropic.com");
}

#[tokio::test]
async fn tunnels_list_parses_page_cursor() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/tunnels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{"id": "tnl_01", "type": "tunnel"}],
            "has_more": false
        })))
        .mount(&server)
        .await;

    let client = build_client(&server);
    let page = client.beta().tunnels().list().await.unwrap();

    assert_eq!(page.data.len(), 1);
    assert_eq!(page.data[0].id, "tnl_01");
}

#[tokio::test]
async fn tunnels_reveal_token_returns_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/tunnels/tnl_01/reveal_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "tok_01",
            "type": "tunnel_token",
            "tunnel_token": "secret-token"
        })))
        .mount(&server)
        .await;

    let client = build_client(&server);
    let token = client
        .beta()
        .tunnels()
        .reveal_token("tnl_01")
        .await
        .unwrap();

    assert_eq!(token.id, "tok_01");
    assert_eq!(token.extra["tunnel_token"], "secret-token");
}

#[tokio::test]
async fn tunnel_certificates_create_uses_nested_path() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/tunnels/tnl_01/certificates"))
        .and(header("anthropic-beta", "mcp-tunnels-2026-06-22"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "cert_01",
            "type": "tunnel_certificate"
        })))
        .mount(&server)
        .await;

    let client = build_client(&server);
    let cert = client
        .beta()
        .with_beta_headers(vec!["mcp-tunnels-2026-06-22".into()])
        .tunnels()
        .certificates()
        .create("tnl_01", &serde_json::json!({"csr": "..."}))
        .await
        .unwrap();

    assert_eq!(cert.id, "cert_01");
}

#[tokio::test]
async fn tunnel_certificates_list_uses_nested_path() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/tunnels/tnl_01/certificates"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{"id": "cert_01", "type": "tunnel_certificate"}],
            "has_more": false
        })))
        .mount(&server)
        .await;

    let client = build_client(&server);
    let page = client
        .beta()
        .tunnels()
        .certificates()
        .list("tnl_01")
        .await
        .unwrap();

    assert_eq!(page.data.len(), 1);
    assert_eq!(page.data[0].id, "cert_01");
}
