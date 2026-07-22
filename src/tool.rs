use std::marker::PhantomData;

use schemars::{JsonSchema, schema_for};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

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

#[derive(Debug, Clone)]
pub struct ToolResult {
    pub call_id: String,
    pub output: ToolOutput,
}

#[derive(Debug, Clone)]
enum ToolOutput {
    Text(String),
    Json(Value),
}

#[async_trait::async_trait]
pub trait TypedTool: Send + Sync + 'static {
    type Arguments: DeserializeOwned + JsonSchema + Send + 'static;

    type Output: Serialize + Send + 'static;

    fn name(&self) -> &'static str;

    fn description(&self) -> &'static str;

    fn definition(&self) -> ToolDefinition;

    async fn call(&self, arguments: Self::Arguments) -> anyhow::Result<Self::Output>;
}

#[async_trait::async_trait]
pub trait DynTool: Send + Sync {
    fn name(&self) -> &str;

    fn description(&self) -> &str;

    fn input_schema(&self) -> Value;

    fn definition(&self) -> ToolDefinition;

    async fn call(&self, arguments: Value) -> anyhow::Result<Value>;
}

#[async_trait::async_trait]
impl<T> DynTool for T
where
    T: TypedTool,
{
    fn name(&self) -> &str {
        TypedTool::name(self)
    }

    fn description(&self) -> &str {
        TypedTool::description(self)
    }

    fn definition(&self) -> ToolDefinition {
        TypedTool::definition(self)
    }

    fn input_schema(&self) -> Value {
        serde_json::to_value(schema_for!(T::Arguments)).expect("JSON Schema should be serializable")
    }

    async fn call(&self, arguments: Value) -> anyhow::Result<Value> {
        let arguments = serde_json::from_value::<T::Arguments>(arguments)?;

        let output = TypedTool::call(self, arguments).await?;

        Ok(serde_json::to_value(output)?)
    }
}
