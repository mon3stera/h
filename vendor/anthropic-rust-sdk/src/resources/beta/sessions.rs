//! Beta Sessions API.

use crate::client::Anthropic;
use crate::core::error::Error;
use crate::core::pagination::PageCursor;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BetaSession {
    pub id: String,
    #[serde(flatten)]
    pub extra: Value,
}

pub struct BetaSessions<'a> {
    client: &'a Anthropic,
    beta_headers: Vec<String>,
}

impl<'a> BetaSessions<'a> {
    pub(crate) fn new(client: &'a Anthropic, beta_headers: Vec<String>) -> Self {
        Self {
            client,
            beta_headers,
        }
    }

    pub async fn list(&self) -> Result<PageCursor<BetaSession>, Error> {
        self.client
            .get_beta("/v1/sessions", &self.beta_headers, None)
            .await
    }

    pub async fn create(&self, body: &Value) -> Result<BetaSession, Error> {
        self.client
            .post_beta("/v1/sessions", body, &self.beta_headers)
            .await
    }

    pub async fn retrieve(&self, session_id: &str) -> Result<BetaSession, Error> {
        self.client
            .get_beta(
                &format!("/v1/sessions/{session_id}"),
                &self.beta_headers,
                None,
            )
            .await
    }
}
