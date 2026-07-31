//! Beta Vaults API.

use crate::client::Anthropic;
use crate::core::error::Error;
use crate::core::pagination::PageCursor;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BetaVault {
    pub id: String,
    #[serde(flatten)]
    pub extra: Value,
}

pub struct BetaVaults<'a> {
    client: &'a Anthropic,
    beta_headers: Vec<String>,
}

impl<'a> BetaVaults<'a> {
    pub(crate) fn new(client: &'a Anthropic, beta_headers: Vec<String>) -> Self {
        Self {
            client,
            beta_headers,
        }
    }

    pub async fn list(&self) -> Result<PageCursor<BetaVault>, Error> {
        self.client
            .get_beta("/v1/vaults", &self.beta_headers, None)
            .await
    }

    pub async fn create(&self, body: &Value) -> Result<BetaVault, Error> {
        self.client
            .post_beta("/v1/vaults", body, &self.beta_headers)
            .await
    }

    pub async fn retrieve(&self, vault_id: &str) -> Result<BetaVault, Error> {
        self.client
            .get_beta(&format!("/v1/vaults/{vault_id}"), &self.beta_headers, None)
            .await
    }
}
