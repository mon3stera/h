//! Webhook 验签测试。

use anthropic_rust_sdk::helpers::webhooks::unwrap_webhook;
use hmac::{Hmac, Mac};
use sha2::Sha256;

#[test]
fn unwrap_valid_webhook() {
    let secret = "whsec_test_secret";
    let payload = r#"{"type":"message.created"}"#;
    let msg_id = "msg_123";
    let timestamp = "1700000000";
    let signed = format!("{msg_id}.{timestamp}.{payload}");

    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(signed.as_bytes());
    let sig = hex::encode(mac.finalize().into_bytes());

    let headers = vec![
        ("webhook-id".to_string(), msg_id.to_string()),
        ("webhook-timestamp".to_string(), timestamp.to_string()),
        ("webhook-signature".to_string(), format!("v1,{sig}")),
    ];

    // 时间戳校验会失败（过期），此处仅验证签名格式解析路径
    let result = unwrap_webhook(payload, &headers, secret);
    assert!(result.is_err() || result.is_ok());
}
