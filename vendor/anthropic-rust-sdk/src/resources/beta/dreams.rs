//! Beta Dreams API（dreaming），对齐上游 `src/resources/beta/dreams.ts`。

use crate::client::Anthropic;
use crate::core::error::Error;
use crate::core::pagination::PageCursor;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Dream 资源对象。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BetaDream {
    pub id: String,
    #[serde(flatten)]
    pub extra: Value,
}

/// Beta Dreams API 资源。
pub struct BetaDreams<'a> {
    client: &'a Anthropic,
    beta_headers: Vec<String>,
}

impl<'a> BetaDreams<'a> {
    pub(crate) fn new(client: &'a Anthropic, beta_headers: Vec<String>) -> Self {
        Self {
            client,
            beta_headers,
        }
    }

    pub async fn create(&self, body: &Value) -> Result<BetaDream, Error> {
        self.client
            .post_beta("/v1/dreams", body, &self.beta_headers)
            .await
    }

    pub async fn retrieve(&self, dream_id: &str) -> Result<BetaDream, Error> {
        self.client
            .get_beta(&format!("/v1/dreams/{dream_id}"), &self.beta_headers, None)
            .await
    }

    pub async fn list(&self) -> Result<PageCursor<BetaDream>, Error> {
        self.client
            .get_beta("/v1/dreams", &self.beta_headers, None)
            .await
    }

    /// 归档一个 dream（无请求体的 POST）。
    pub async fn archive(&self, dream_id: &str) -> Result<BetaDream, Error> {
        self.client
            .post_beta(
                &format!("/v1/dreams/{dream_id}/archive"),
                &serde_json::json!({}),
                &self.beta_headers,
            )
            .await
    }

    /// 取消一个进行中的 dream（无请求体的 POST）。
    pub async fn cancel(&self, dream_id: &str) -> Result<BetaDream, Error> {
        self.client
            .post_beta(
                &format!("/v1/dreams/{dream_id}/cancel"),
                &serde_json::json!({}),
                &self.beta_headers,
            )
            .await
    }
}
