//! Beta Environments API.

use crate::client::Anthropic;
use crate::core::error::Error;
use crate::core::pagination::PageCursor;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BetaEnvironment {
    pub id: String,
    #[serde(flatten)]
    pub extra: Value,
}

pub struct BetaEnvironments<'a> {
    client: &'a Anthropic,
    beta_headers: Vec<String>,
}

impl<'a> BetaEnvironments<'a> {
    pub(crate) fn new(client: &'a Anthropic, beta_headers: Vec<String>) -> Self {
        Self {
            client,
            beta_headers,
        }
    }

    pub async fn list(&self) -> Result<PageCursor<BetaEnvironment>, Error> {
        self.client
            .get_beta("/v1/environments", &self.beta_headers, None)
            .await
    }

    pub async fn create(&self, body: &Value) -> Result<BetaEnvironment, Error> {
        self.client
            .post_beta("/v1/environments", body, &self.beta_headers)
            .await
    }

    pub async fn retrieve(&self, environment_id: &str) -> Result<BetaEnvironment, Error> {
        self.client
            .get_beta(
                &format!("/v1/environments/{environment_id}"),
                &self.beta_headers,
                None,
            )
            .await
    }
}
