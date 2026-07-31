//! Beta Skills API.

use crate::client::Anthropic;
use crate::core::error::Error;
use crate::core::pagination::PageCursor;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BetaSkill {
    pub id: String,
    #[serde(rename = "type")]
    pub object_type: String,
    #[serde(flatten)]
    pub extra: Value,
}

pub struct BetaSkills<'a> {
    client: &'a Anthropic,
    beta_headers: Vec<String>,
}

impl<'a> BetaSkills<'a> {
    pub(crate) fn new(client: &'a Anthropic, beta_headers: Vec<String>) -> Self {
        Self {
            client,
            beta_headers,
        }
    }

    pub async fn list(&self) -> Result<PageCursor<BetaSkill>, Error> {
        self.client
            .get_beta("/v1/skills", &self.beta_headers, None)
            .await
    }

    pub async fn create(&self, body: &Value) -> Result<BetaSkill, Error> {
        self.client
            .post_beta("/v1/skills", body, &self.beta_headers)
            .await
    }

    pub async fn retrieve(&self, skill_id: &str) -> Result<BetaSkill, Error> {
        self.client
            .get_beta(&format!("/v1/skills/{skill_id}"), &self.beta_headers, None)
            .await
    }
}
