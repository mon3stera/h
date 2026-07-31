//! Beta Agents API.

use crate::client::Anthropic;
use crate::core::error::Error;
use crate::core::pagination::PageCursor;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BetaAgent {
    pub id: String,
    #[serde(flatten)]
    pub extra: Value,
}

pub struct BetaAgents<'a> {
    client: &'a Anthropic,
    beta_headers: Vec<String>,
}

impl<'a> BetaAgents<'a> {
    pub(crate) fn new(client: &'a Anthropic, beta_headers: Vec<String>) -> Self {
        Self {
            client,
            beta_headers,
        }
    }

    pub async fn list(&self) -> Result<PageCursor<BetaAgent>, Error> {
        self.client
            .get_beta("/v1/agents", &self.beta_headers, None)
            .await
    }

    pub async fn create(&self, body: &Value) -> Result<BetaAgent, Error> {
        self.client
            .post_beta("/v1/agents", body, &self.beta_headers)
            .await
    }

    pub async fn retrieve(&self, agent_id: &str) -> Result<BetaAgent, Error> {
        self.client
            .get_beta(&format!("/v1/agents/{agent_id}"), &self.beta_headers, None)
            .await
    }
}
