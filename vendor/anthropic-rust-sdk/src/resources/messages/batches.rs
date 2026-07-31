//! Messages Batches API。

use crate::client::Anthropic;
use crate::core::error::Error;
use crate::core::pagination::PageCursor;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 批处理任务。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MessageBatch {
    pub id: String,
    #[serde(rename = "type")]
    pub object_type: String,
    pub processing_status: String,
    #[serde(flatten)]
    pub extra: Value,
}

/// 创建批处理请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageBatchCreateParams {
    pub requests: Vec<Value>,
    #[serde(flatten)]
    pub extra: Value,
}

/// Messages Batches 资源。
pub struct Batches<'a> {
    client: &'a Anthropic,
}

impl<'a> Batches<'a> {
    pub(crate) fn new(client: &'a Anthropic) -> Self {
        Self { client }
    }

    pub async fn create(&self, params: MessageBatchCreateParams) -> Result<MessageBatch, Error> {
        self.client.post("/v1/messages/batches", &params).await
    }

    pub async fn retrieve(&self, batch_id: &str) -> Result<MessageBatch, Error> {
        self.client
            .get(&format!("/v1/messages/batches/{batch_id}"))
            .await
    }

    pub async fn list(
        &self,
        query: Option<&[(&str, &str)]>,
    ) -> Result<PageCursor<MessageBatch>, Error> {
        self.client
            .get_with_query("/v1/messages/batches", query)
            .await
    }

    pub async fn cancel(&self, batch_id: &str) -> Result<MessageBatch, Error> {
        self.client
            .post_empty(&format!("/v1/messages/batches/{batch_id}/cancel"))
            .await
    }

    pub async fn results(&self, batch_id: &str) -> Result<Value, Error> {
        self.client
            .get(&format!("/v1/messages/batches/{batch_id}/results"))
            .await
    }
}
