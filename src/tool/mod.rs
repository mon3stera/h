use std::marker::PhantomData;

use schemars::{JsonSchema, schema_for};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

mod ask;
mod bash;
mod edit;
mod fetch;
mod file_buffer;
mod grep;
mod presentation;
mod read;
mod registry;
mod write;

pub use ask::AskTool;
pub use bash::{BashPresenter, BashTool};
pub use edit::EditTool;
pub use fetch::{FetchPresenter, FetchTool};
pub use file_buffer::FileBufferStore;
pub use grep::{GrepPresenter, GrepTool};
pub use presentation::{
    DefaultPresenter, DisplayBlock, KeyValueEntry, Presentation, Presenter, ToolCallStatus,
};
pub use read::{ReadFilePresenter, ReadFileTool};
pub use registry::ToolRegistry;
pub use write::{WriteFilePresenter, WriteFileTool};

#[cfg(test)]
pub use bash::{BashToolArgs, BashToolOutput};
#[cfg(test)]
pub use read::ReadFileToolArgs;
#[cfg(test)]
pub use write::{WriteFileMode, WriteFileToolArgs};

#[derive(Debug, Clone)]
pub struct ToolSpec<T> {
    pub name: String,
    pub description: String,
    _arguments: PhantomData<fn() -> T>,
}

impl<T> ToolSpec<T>
where
    T: JsonSchema,
{
    fn erase(self) -> anyhow::Result<ToolDefinition> {
        let schema = serde_json::to_value(schemars::schema_for!(T))?;

        Ok(ToolDefinition {
            name: self.name,
            description: self.description,
            arguments: schema,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub arguments: Value,
}

#[async_trait::async_trait]
pub trait TypedTool: Send + Sync + 'static {
    type Arguments: DeserializeOwned + JsonSchema + Send + 'static;
    type Output: Serialize + Send + 'static;

    fn name(&self) -> &'static str;

    fn description(&self) -> &'static str;

    fn definition(&self) -> anyhow::Result<ToolDefinition> {
        let spec: ToolSpec<Self::Arguments> = ToolSpec {
            name: self.name().to_owned(),
            description: self.description().to_owned(),
            _arguments: PhantomData,
        };

        spec.erase()
    }

    async fn call(&self, arguments: Self::Arguments) -> anyhow::Result<Self::Output>;
}

#[async_trait::async_trait]
pub trait DynTool: Send + Sync {
    fn name(&self) -> &'static str;

    fn description(&self) -> &'static str;

    fn input_schema(&self) -> Value;

    fn definition(&self) -> anyhow::Result<ToolDefinition>;

    async fn call(&self, arguments: Value) -> anyhow::Result<Value>;
}

#[async_trait::async_trait]
impl<T> DynTool for T
where
    T: TypedTool,
{
    fn name(&self) -> &'static str {
        TypedTool::name(self)
    }

    fn description(&self) -> &'static str {
        TypedTool::description(self)
    }

    fn input_schema(&self) -> Value {
        serde_json::to_value(schema_for!(T::Arguments)).expect("JSON Schema should be serializable")
    }

    fn definition(&self) -> anyhow::Result<ToolDefinition> {
        TypedTool::definition(self)
    }

    async fn call(&self, arguments: Value) -> anyhow::Result<Value> {
        let arguments = serde_json::from_value::<T::Arguments>(arguments)?;
        let output = TypedTool::call(self, arguments).await?;

        Ok(serde_json::to_value(output)?)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ToolCallId(pub String);

impl ToolCallId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for ToolCallId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for ToolCallId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

#[derive(Debug, Clone)]
pub struct ToolCall {
    id: ToolCallId,
    name: String,
    arguments: Value,
}

impl ToolCall {
    pub fn new(id: impl Into<ToolCallId>, name: impl Into<String>, arguments: Value) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }

    pub fn id(&self) -> &ToolCallId {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn arguments(&self) -> &Value {
        &self.arguments
    }
}

#[derive(Debug, Clone)]
pub enum ToolCallOutcome {
    Success(Value),
    Failure { message: String },
}

#[derive(Debug, Clone)]
pub struct ToolCallResult {
    id: ToolCallId,
    outcome: ToolCallOutcome,
}

impl ToolCallResult {
    pub fn success(id: impl Into<ToolCallId>, output: Value) -> Self {
        Self {
            id: id.into(),
            outcome: ToolCallOutcome::Success(output),
        }
    }

    pub fn failure(id: impl Into<ToolCallId>, message: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            outcome: ToolCallOutcome::Failure {
                message: message.into(),
            },
        }
    }

    pub fn id(&self) -> &ToolCallId {
        &self.id
    }

    pub fn outcome(&self) -> &ToolCallOutcome {
        &self.outcome
    }

    pub fn into_provider_output(self) -> String {
        match self.outcome {
            ToolCallOutcome::Success(output) => serde_json::to_string(&output)
                .unwrap_or_else(|error| format!("Failed to serialize tool output: {error}")),
            ToolCallOutcome::Failure { message } => serde_json::json!({
                "error": message,
            })
            .to_string(),
        }
    }
}

#[cfg(test)]
use presentation::{MAX_ERROR_CHARS, REDACTED, humanize_tool_name};
#[cfg(test)]
use read::MAX_READ_LINES;

#[cfg(test)]
mod tests;
