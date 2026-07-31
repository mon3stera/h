//! Beta Deployment Runs API.

use crate::client::Anthropic;
use crate::core::error::Error;
use crate::core::pagination::PageCursor;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BetaDeploymentRun {
    pub id: String,
    #[serde(flatten)]
    pub extra: Value,
}

pub struct BetaDeploymentRuns<'a> {
    client: &'a Anthropic,
    beta_headers: Vec<String>,
}

impl<'a> BetaDeploymentRuns<'a> {
    pub(crate) fn new(client: &'a Anthropic, beta_headers: Vec<String>) -> Self {
        Self {
            client,
            beta_headers,
        }
    }

    pub async fn list(&self) -> Result<PageCursor<BetaDeploymentRun>, Error> {
        self.client
            .get_beta("/v1/deployment_runs", &self.beta_headers, None)
            .await
    }

    pub async fn retrieve(&self, run_id: &str) -> Result<BetaDeploymentRun, Error> {
        self.client
            .get_beta(
                &format!("/v1/deployment_runs/{run_id}"),
                &self.beta_headers,
                None,
            )
            .await
    }
}
