//! Beta Memory Stores API.

use crate::client::Anthropic;
use crate::core::error::Error;
use crate::core::pagination::PageCursor;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BetaMemoryStore {
    pub id: String,
    #[serde(flatten)]
    pub extra: Value,
}

pub struct BetaMemoryStores<'a> {
    client: &'a Anthropic,
    beta_headers: Vec<String>,
}

impl<'a> BetaMemoryStores<'a> {
    pub(crate) fn new(client: &'a Anthropic, beta_headers: Vec<String>) -> Self {
        Self {
            client,
            beta_headers,
        }
    }

    pub async fn list(&self) -> Result<PageCursor<BetaMemoryStore>, Error> {
        self.client
            .get_beta("/v1/memory_stores", &self.beta_headers, None)
            .await
    }

    pub async fn create(&self, body: &Value) -> Result<BetaMemoryStore, Error> {
        self.client
            .post_beta("/v1/memory_stores", body, &self.beta_headers)
            .await
    }

    pub async fn retrieve(&self, store_id: &str) -> Result<BetaMemoryStore, Error> {
        self.client
            .get_beta(
                &format!("/v1/memory_stores/{store_id}"),
                &self.beta_headers,
                None,
            )
            .await
    }
}
