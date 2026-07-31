//! Beta User Profiles API.

use crate::client::Anthropic;
use crate::core::error::Error;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BetaUserProfile {
    pub id: String,
    #[serde(flatten)]
    pub extra: Value,
}

pub struct BetaUserProfiles<'a> {
    client: &'a Anthropic,
    beta_headers: Vec<String>,
}

impl<'a> BetaUserProfiles<'a> {
    pub(crate) fn new(client: &'a Anthropic, beta_headers: Vec<String>) -> Self {
        Self {
            client,
            beta_headers,
        }
    }

    pub async fn retrieve(&self) -> Result<BetaUserProfile, Error> {
        self.client
            .get_beta("/v1/user_profiles/me", &self.beta_headers, None)
            .await
    }
}
