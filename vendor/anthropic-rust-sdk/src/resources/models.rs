//! Models API，对齐上游 `src/resources/models.ts`。

use crate::client::Anthropic;
use crate::core::error::Error;
use crate::core::pagination::TokenPage;
use serde::{Deserialize, Serialize};
use serde_json::Value;

fn default_model_object_type() -> String {
    "model".to_string()
}

/// 模型信息。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelInfo {
    pub id: String,
    #[serde(rename = "type", default = "default_model_object_type")]
    pub object_type: String,
    #[serde(default, alias = "name")]
    pub display_name: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default, flatten)]
    pub extra: Value,
}

impl ModelInfo {
    /// 展示名；缺失或为空时回退为 `id`（兼容部分 Anthropic 兼容网关）。
    pub fn effective_display_name(&self) -> &str {
        if self.display_name.trim().is_empty() {
            &self.id
        } else {
            &self.display_name
        }
    }
}

/// Models API 资源。
pub struct Models<'a> {
    client: &'a Anthropic,
}

impl<'a> Models<'a> {
    pub(crate) fn new(client: &'a Anthropic) -> Self {
        Self { client }
    }

    pub async fn list(
        &self,
        query: Option<&[(&str, &str)]>,
    ) -> Result<TokenPage<ModelInfo>, Error> {
        self.client.get_with_query("/v1/models", query).await
    }

    pub async fn retrieve(&self, model_id: &str) -> Result<ModelInfo, Error> {
        self.client.get(&format!("/v1/models/{model_id}")).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_info_minimal_json() {
        let raw = r#"{"id":"glm-5.2"}"#;
        let info: ModelInfo = serde_json::from_str(raw).unwrap();
        assert_eq!(info.id, "glm-5.2");
        assert_eq!(info.object_type, "model");
        assert_eq!(info.effective_display_name(), "glm-5.2");
    }

    #[test]
    fn model_info_with_display_name() {
        let raw = r#"{"id":"glm-5.2","display_name":"GLM 5.2","type":"model"}"#;
        let info: ModelInfo = serde_json::from_str(raw).unwrap();
        assert_eq!(info.effective_display_name(), "GLM 5.2");
    }
}
