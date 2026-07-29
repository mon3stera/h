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
mod output;
mod presentation;
mod read;
mod registry;
mod summary;
mod write;

pub use ask::{AskPresenter, AskTool};
pub use bash::{BashPresenter, BashTool};
pub use edit::{EditPresenter, EditTool};
pub use fetch::{FetchPresenter, FetchTool};
pub use file_buffer::FileBufferStore;
pub use grep::{GrepPresenter, GrepTool};
pub use presentation::{
    DefaultPresenter, DiffLine, DiffLineKind, DisplayBlock, KeyValueEntry, Presentation, Presenter,
    ToolCallStatus,
};
pub use read::{ReadFilePresenter, ReadFileTool};
pub use registry::ToolRegistry;
pub use summary::{Aggregator, Summary};
pub use write::{WriteFilePresenter, WriteFileTool};

#[cfg(test)]
pub use bash::{BashToolArgs, BashToolOutput};
#[cfg(test)]
pub use edit::EditToolArgs;
#[cfg(test)]
pub use fetch::FetchToolArgs;
#[cfg(test)]
pub use grep::GrepToolArgs;
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

    async fn call(&self, arguments: Self::Arguments) -> anyhow::Result<ToolOutput<Self::Output>>;

    /// Creates fresh aggregation state. `None` makes this tool a hard boundary
    /// between aggregatable runs.
    fn aggregator(&self) -> Option<Box<dyn Aggregator>> {
        None
    }

    /// Stops external work started by the current call. Most async tools need
    /// no hook because dropping their call future is sufficient; tools that own
    /// subprocesses or other out-of-process work should override this method.
    /// The registry invokes it only after the call future has been dropped.
    async fn cancel(&self, _arguments: Self::Arguments) -> anyhow::Result<()> {
        Ok(())
    }
}

#[async_trait::async_trait]
pub trait DynTool: Send + Sync {
    fn name(&self) -> &'static str;

    fn description(&self) -> &'static str;

    fn input_schema(&self) -> Value;

    fn definition(&self) -> anyhow::Result<ToolDefinition>;

    async fn call(&self, arguments: Value) -> anyhow::Result<ToolOutput<Value>>;

    fn aggregator(&self) -> Option<Box<dyn Aggregator>>;

    async fn cancel(&self, arguments: Value) -> anyhow::Result<()>;
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

    async fn call(&self, arguments: Value) -> anyhow::Result<ToolOutput<Value>> {
        let arguments = serde_json::from_value::<T::Arguments>(arguments)?;
        let output = TypedTool::call(self, arguments).await?;
        let (value, summary) = output.into_parts();

        Ok(ToolOutput {
            value: serde_json::to_value(value)?,
            summary,
        })
    }

    fn aggregator(&self) -> Option<Box<dyn Aggregator>> {
        TypedTool::aggregator(self)
    }

    async fn cancel(&self, arguments: Value) -> anyhow::Result<()> {
        let arguments = serde_json::from_value::<T::Arguments>(arguments)?;
        TypedTool::cancel(self, arguments).await
    }
}

#[derive(Debug)]
pub struct ToolOutput<T> {
    value: T,
    summary: Option<Summary>,
}

impl<T> ToolOutput<T> {
    pub fn new(value: T) -> Self {
        Self {
            value,
            summary: None,
        }
    }

    pub fn with_summary(mut self, summary: Summary) -> Self {
        self.summary = Some(summary);
        self
    }

    pub fn value(&self) -> &T {
        &self.value
    }

    pub fn summary(&self) -> Option<&Summary> {
        self.summary.as_ref()
    }

    pub fn into_value(self) -> T {
        self.value
    }

    fn into_parts(self) -> (T, Option<Summary>) {
        (self.value, self.summary)
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
    summary: Option<Summary>,
}

impl ToolCallResult {
    pub fn success(id: impl Into<ToolCallId>, output: Value) -> Self {
        Self {
            id: id.into(),
            outcome: ToolCallOutcome::Success(output),
            summary: None,
        }
    }

    pub fn success_with_summary(
        id: impl Into<ToolCallId>,
        output: Value,
        summary: Option<Summary>,
    ) -> Self {
        Self {
            id: id.into(),
            outcome: ToolCallOutcome::Success(output),
            summary,
        }
    }

    pub fn failure(id: impl Into<ToolCallId>, message: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            outcome: ToolCallOutcome::Failure {
                message: message.into(),
            },
            summary: None,
        }
    }

    pub fn id(&self) -> &ToolCallId {
        &self.id
    }

    pub fn outcome(&self) -> &ToolCallOutcome {
        &self.outcome
    }

    pub fn summary(&self) -> Option<&Summary> {
        self.summary.as_ref()
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
use read::{MAX_READ_CHARS, MAX_READ_LINES};

#[cfg(test)]
mod tests;
