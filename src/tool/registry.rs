use std::{collections::HashMap, time::Instant};

use super::{
    Aggregator, DefaultPresenter, DynTool, Presentation, Presenter, ToolCall, ToolCallResult,
    ToolDefinition, TypedTool,
};

struct RegisteredTool {
    tool: Box<dyn DynTool>,
    presenter: Box<dyn Presenter>,
}

pub struct ToolRegistry {
    tools: HashMap<&'static str, RegisteredTool>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register<T: TypedTool>(&mut self, tool: T) -> &mut Self {
        self.register_with_presenter(tool, DefaultPresenter)
    }

    pub fn register_with_presenter<T, P>(&mut self, tool: T, presenter: P) -> &mut Self
    where
        T: TypedTool,
        P: Presenter + 'static,
    {
        let name = tool.name();
        let replaced = self
            .tools
            .insert(
                name,
                RegisteredTool {
                    tool: Box::new(tool),
                    presenter: Box::new(presenter),
                },
            )
            .is_some();

        tracing::debug!(
            event = "tool.registered",
            tool_name = name,
            replaced,
            tool_count = self.tools.len()
        );
        self
    }

    pub fn definitions(&self) -> anyhow::Result<Vec<ToolDefinition>> {
        let definitions = self
            .tools
            .values()
            .map(|registered| registered.tool.definition())
            .collect::<anyhow::Result<Vec<_>>>()?;

        tracing::debug!(
            event = "tool.definitions.generated",
            tool_count = definitions.len()
        );
        Ok(definitions)
    }

    pub fn present_running(&self, call: &ToolCall) -> Presentation {
        self.tools
            .get(call.name())
            .map(|registered| registered.presenter.running(call))
            .unwrap_or_else(|| DefaultPresenter.running(call))
    }

    pub fn present_completed(&self, call: &ToolCall, result: &ToolCallResult) -> Presentation {
        self.tools
            .get(call.name())
            .map(|registered| registered.presenter.completed(call, result))
            .unwrap_or_else(|| DefaultPresenter.completed(call, result))
    }

    pub fn aggregator(&self, name: &str) -> Option<Box<dyn Aggregator>> {
        self.tools
            .get(name)
            .and_then(|registered| registered.tool.aggregator())
    }

    pub async fn call(&self, call: &ToolCall) -> ToolCallResult {
        let started = Instant::now();
        let span = tracing::info_span!("tool.call", tool_name = call.name());
        let _guard = span.enter();

        tracing::info!(event = "tool.call.started");

        let Some(registered) = self.tools.get(call.name()) else {
            tracing::warn!(
                event = "tool.call.completed",
                outcome = "failure",
                error_class = "unknown_tool",
                duration_ms = started.elapsed().as_millis() as u64
            );
            return ToolCallResult::failure(
                call.id().clone(),
                format!("Failed to find tool: {}", call.name()),
            );
        };

        match registered.tool.call(call.arguments().clone()).await {
            Ok(output) => {
                tracing::info!(
                    event = "tool.call.completed",
                    outcome = "success",
                    duration_ms = started.elapsed().as_millis() as u64
                );
                ToolCallResult::success_with_summary(
                    call.id().clone(),
                    output.value,
                    output.summary,
                )
            }
            Err(error) => {
                tracing::warn!(
                    event = "tool.call.completed",
                    outcome = "failure",
                    error_class = "tool_execution_error",
                    duration_ms = started.elapsed().as_millis() as u64
                );
                ToolCallResult::failure(call.id().clone(), error.to_string())
            }
        }
    }

    pub async fn cancel(&self, call: &ToolCall) -> anyhow::Result<()> {
        let Some(registered) = self.tools.get(call.name()) else {
            return Ok(());
        };

        tracing::info!(event = "tool.cancel.started", tool_name = call.name());
        registered.tool.cancel(call.arguments().clone()).await?;
        tracing::info!(event = "tool.cancel.completed", tool_name = call.name());

        Ok(())
    }
}
