//! Beta Models API.

use crate::client::Anthropic;
use crate::core::error::Error;
use crate::core::pagination::TokenPage;
use crate::resources::models::ModelInfo;

pub struct BetaModels<'a> {
    client: &'a Anthropic,
    beta_headers: Vec<String>,
}

impl<'a> BetaModels<'a> {
    pub(crate) fn new(client: &'a Anthropic, beta_headers: Vec<String>) -> Self {
        Self {
            client,
            beta_headers,
        }
    }

    pub async fn list(&self) -> Result<TokenPage<ModelInfo>, Error> {
        self.client
            .get_beta("/v1/models", &self.beta_headers, None)
            .await
    }
}
