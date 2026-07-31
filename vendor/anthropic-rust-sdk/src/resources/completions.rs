//! Legacy Completions API，对齐上游 `src/resources/completions.ts`。

use crate::client::Anthropic;
use crate::core::error::Error;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Completion 响应。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Completion {
    pub id: String,
    pub completion: String,
    pub model: String,
    pub stop_reason: Option<String>,
    #[serde(flatten)]
    pub extra: Value,
}

/// 创建 Completion 请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionCreateParams {
    pub model: String,
    pub max_tokens_to_sample: u64,
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(flatten)]
    pub extra: Value,
}

/// Completions API 资源（legacy）。
pub struct Completions<'a> {
    client: &'a Anthropic,
}

impl<'a> Completions<'a> {
    pub(crate) fn new(client: &'a Anthropic) -> Self {
        Self { client }
    }

    pub async fn create(&self, params: CompletionCreateParams) -> Result<Completion, Error> {
        self.client.post("/v1/complete", &params).await
    }
}
