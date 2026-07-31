//! Webhook 验签，对齐上游 webhook unwrap 行为。

use crate::core::error::{AnthropicError, Error};
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

/// Webhook unwrap 结果。
#[derive(Debug, Clone)]
pub struct UnwrapWebhookResult {
    pub payload: Value,
    pub timestamp: i64,
}

/// 验证并解析 webhook 载荷。
pub fn unwrap_webhook(
    payload: &str,
    headers: &[(String, String)],
    secret: &str,
) -> Result<UnwrapWebhookResult, Error> {
    let msg_id = header_value(headers, "webhook-id")
        .ok_or_else(|| AnthropicError("missing webhook-id header".into()))?;
    let timestamp = header_value(headers, "webhook-timestamp")
        .ok_or_else(|| AnthropicError("missing webhook-timestamp header".into()))?;
    let signature = header_value(headers, "webhook-signature")
        .ok_or_else(|| AnthropicError("missing webhook-signature header".into()))?;

    let ts: i64 = timestamp
        .parse()
        .map_err(|_| AnthropicError("invalid webhook-timestamp".into()))?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    if (now - ts).abs() > 300 {
        return Err(AnthropicError("webhook timestamp too old".into()).into());
    }

    let signed_content = format!("{msg_id}.{timestamp}.{payload}");
    verify_signature(&signed_content, secret, &signature)?;

    let value: Value = serde_json::from_str(payload)
        .map_err(|e| AnthropicError(format!("invalid webhook JSON payload: {e}")))?;

    Ok(UnwrapWebhookResult {
        payload: value,
        timestamp: ts,
    })
}

fn header_value(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
}

fn verify_signature(
    signed_content: &str,
    secret: &str,
    signature_header: &str,
) -> Result<(), Error> {
    for part in signature_header.split(' ') {
        if let Some(hex) = part.strip_prefix("v1,") {
            let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
                .map_err(|e| AnthropicError(format!("invalid webhook secret: {e}")))?;
            mac.update(signed_content.as_bytes());
            let expected = mac.finalize().into_bytes();
            let provided = hex::decode(hex)
                .map_err(|e| AnthropicError(format!("invalid webhook signature encoding: {e}")))?;
            if expected.as_slice() == provided.as_slice() {
                return Ok(());
            }
        }
    }
    Err(AnthropicError("webhook signature verification failed".into()).into())
}
