//! Beta Files API.

use crate::client::Anthropic;
use crate::core::error::Error;
use crate::core::pagination::PageCursor;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BetaFile {
    pub id: String,
    #[serde(rename = "type")]
    pub object_type: String,
    #[serde(flatten)]
    pub extra: Value,
}

pub struct BetaFiles<'a> {
    client: &'a Anthropic,
    beta_headers: Vec<String>,
}

impl<'a> BetaFiles<'a> {
    pub(crate) fn new(client: &'a Anthropic, beta_headers: Vec<String>) -> Self {
        Self {
            client,
            beta_headers,
        }
    }

    pub async fn list(&self) -> Result<PageCursor<BetaFile>, Error> {
        self.client
            .get_beta("/v1/files", &self.beta_headers, None)
            .await
    }

    pub async fn retrieve(&self, file_id: &str) -> Result<BetaFile, Error> {
        self.client
            .get_beta(&format!("/v1/files/{file_id}"), &self.beta_headers, None)
            .await
    }

    pub async fn delete(&self, file_id: &str) -> Result<BetaFile, Error> {
        self.client
            .delete_beta(&format!("/v1/files/{file_id}"), &self.beta_headers)
            .await
    }
}
