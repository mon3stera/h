//! Beta Deployments API.

use crate::client::Anthropic;
use crate::core::error::Error;
use crate::core::pagination::PageCursor;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BetaDeployment {
    pub id: String,
    #[serde(flatten)]
    pub extra: Value,
}

pub struct BetaDeployments<'a> {
    client: &'a Anthropic,
    beta_headers: Vec<String>,
}

impl<'a> BetaDeployments<'a> {
    pub(crate) fn new(client: &'a Anthropic, beta_headers: Vec<String>) -> Self {
        Self {
            client,
            beta_headers,
        }
    }

    pub async fn list(&self) -> Result<PageCursor<BetaDeployment>, Error> {
        self.client
            .get_beta("/v1/deployments", &self.beta_headers, None)
            .await
    }

    pub async fn create(&self, body: &Value) -> Result<BetaDeployment, Error> {
        self.client
            .post_beta("/v1/deployments", body, &self.beta_headers)
            .await
    }

    pub async fn retrieve(&self, deployment_id: &str) -> Result<BetaDeployment, Error> {
        self.client
            .get_beta(
                &format!("/v1/deployments/{deployment_id}"),
                &self.beta_headers,
                None,
            )
            .await
    }
}
