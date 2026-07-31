//! 请求头构建。

use crate::internal::{ANTHROPIC_VERSION, SDK_VERSION};
use http::HeaderMap;
use std::collections::HashMap;

/// 构建默认 API 请求头。
pub fn default_headers(api_key: Option<&str>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Some(api_key) = api_key {
        headers.insert("x-api-key", api_key.parse().unwrap());
    }

    headers.insert("anthropic-version", ANTHROPIC_VERSION.parse().unwrap());
    headers.insert(
        "user-agent",
        format!("anthropic-rust-sdk/{SDK_VERSION}").parse().unwrap(),
    );
    headers.insert("x-stainless-lang", "rust".parse().unwrap());
    headers.insert("x-stainless-package-version", SDK_VERSION.parse().unwrap());
    headers.insert("x-stainless-runtime", "tokio".parse().unwrap());
    headers
}

/// 合并额外请求头。
pub fn merge_headers(base: &mut HeaderMap, extra: &HashMap<String, String>) {
    for (k, v) in extra {
        if let (Ok(name), Ok(value)) = (
            k.parse::<http::HeaderName>(),
            v.parse::<http::HeaderValue>(),
        ) {
            base.insert(name, value);
        }
    }
}
