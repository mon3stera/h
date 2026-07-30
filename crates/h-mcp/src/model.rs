use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icons: Option<Vec<Value>>,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

impl TryFrom<rmcp::model::Tool> for Tool {
    type Error = anyhow::Error;

    fn try_from(tool: rmcp::model::Tool) -> Result<Self> {
        let value = serde_json::to_value(tool).context("failed to serialize MCP tool metadata")?;
        serde_json::from_value(value).context("failed to convert MCP tool metadata")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    value: Value,
    is_error: bool,
}

impl Output {
    pub fn value(&self) -> &Value {
        &self.value
    }

    pub fn is_error(&self) -> bool {
        self.is_error
    }

    pub fn into_value(self) -> Value {
        self.value
    }
}

impl TryFrom<rmcp::model::CallToolResult> for Output {
    type Error = anyhow::Error;

    fn try_from(result: rmcp::model::CallToolResult) -> Result<Self> {
        let is_error = result.is_error.unwrap_or(false);
        let value = serde_json::to_value(result).context("failed to serialize MCP tool output")?;

        Ok(Self { value, is_error })
    }
}
