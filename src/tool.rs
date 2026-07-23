use std::{collections::HashMap, marker::PhantomData, time::Instant};

use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use tokio::fs;

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
        let sepc: ToolSpec<Self::Arguments> = ToolSpec {
            name: self.name().to_string(),
            description: self.description().to_string(),
            _arguments: PhantomData,
        };

        sepc.erase()
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

    fn definition(&self) -> anyhow::Result<ToolDefinition> {
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

#[derive(Deserialize, JsonSchema)]
pub struct ReadFileToolArgs {
    /// File path
    path: String,
}

#[derive(Serialize)]
pub struct ReadFileToolOutput {
    content: String,
}

pub struct ReadFileTool;

#[async_trait::async_trait]
impl TypedTool for ReadFileTool {
    type Arguments = ReadFileToolArgs;

    type Output = ReadFileToolOutput;

    fn name(&self) -> &'static str {
        "read_file"
    }

    fn description(&self) -> &'static str {
        "get file content"
    }

    async fn call(&self, arguments: Self::Arguments) -> anyhow::Result<Self::Output> {
        let content = fs::read_to_string(arguments.path).await?;
        Ok(ReadFileToolOutput { content })
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct WriteFileToolArgs {
    path: String,
    content: String,
}

#[derive(Serialize)]
pub struct WriteFileToolOutput {
    status: String,
}

pub struct WriteFileTool;

#[async_trait::async_trait]
impl TypedTool for WriteFileTool {
    type Arguments = WriteFileToolArgs;

    type Output = WriteFileToolOutput;

    fn name(&self) -> &'static str {
        "write_file"
    }

    fn description(&self) -> &'static str {
        "overwrite a file"
    }

    async fn call(&self, arguments: Self::Arguments) -> anyhow::Result<Self::Output> {
        let status = match fs::write(arguments.path, arguments.content)
            .await
            .map(|_| "Ok".to_string())
            .map_err(|e| e.to_string())
        {
            Ok(s) | Err(s) => s,
        };

        Ok(WriteFileToolOutput { status })
    }
}

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
                ToolCallResult::success(call.id().clone(), output)
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

pub trait Presenter: Send + Sync {
    fn running(&self, call: &ToolCall) -> Presentation;

    fn completed(&self, call: &ToolCall, result: &ToolCallResult) -> Presentation;
}

#[derive(Debug, Clone)]
pub enum ToolCallStatus {
    Running,
    Succeeded,
    Failed { message: String },
}

#[derive(Debug, Clone)]
pub struct KeyValueEntry {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone)]
pub enum DisplayBlock {
    Summary(String),
    CodeBlock {
        language: Option<String>,
        content: String,
        truncated_lines: usize,
    },
    Diff {
        content: String,
        truncated_lines: usize,
    },
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    KeyValue {
        entries: Vec<KeyValueEntry>,
    },
    TextOutput {
        content: String,
        truncated_lines: usize,
    },
}

#[derive(Debug, Clone)]
pub struct Presentation {
    pub call_id: ToolCallId,
    pub name: String,
    pub label: String,
    pub target: Option<String>,
    pub status: ToolCallStatus,
    pub blocks: Vec<DisplayBlock>,
}

pub struct DefaultPresenter;

const MAX_PREVIEW_LINES: usize = 20;
const MAX_PREVIEW_CHARS: usize = 4_000;
const MAX_FIELD_CHARS: usize = 160;
const MAX_ERROR_CHARS: usize = 500;
const REDACTED: &str = "[REDACTED]";

fn humanize_tool_name(name: &str) -> String {
    let words = name
        .split(|character: char| character == '_' || character == '-' || character.is_whitespace())
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut characters = word.chars();
            match characters.next() {
                Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>();

    if words.is_empty() {
        "Tool".to_owned()
    } else {
        words.join(" ")
    }
}

fn is_sensitive_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().replace('-', "_").as_str(),
        "password"
            | "passwd"
            | "secret"
            | "token"
            | "api_key"
            | "apikey"
            | "access_token"
            | "refresh_token"
            | "authorization"
            | "cookie"
            | "set_cookie"
            | "credential"
            | "credentials"
            | "private_key"
    )
}

fn redact_sensitive(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    let value = if is_sensitive_key(key) {
                        Value::String(REDACTED.to_owned())
                    } else {
                        redact_sensitive(value)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(redact_sensitive).collect()),
        value => value.clone(),
    }
}

fn truncate_chars(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_owned();
    }

    let mut output = input.chars().take(max_chars).collect::<String>();
    output.push_str("… [truncated]");
    output
}

fn truncate_preview(input: &str) -> (String, usize) {
    let lines = input.lines().collect::<Vec<_>>();
    let visible_lines = lines.len().min(MAX_PREVIEW_LINES);
    let mut output = lines[..visible_lines].join("\n");
    let omitted_lines = lines.len().saturating_sub(visible_lines);
    let was_char_truncated = output.chars().count() > MAX_PREVIEW_CHARS;

    if was_char_truncated {
        output = output.chars().take(MAX_PREVIEW_CHARS).collect();
    }

    if omitted_lines > 0 || was_char_truncated {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str("… [truncated]");
    }

    (output, omitted_lines + usize::from(was_char_truncated))
}

fn format_field_value(value: &Value) -> String {
    let formatted = match value {
        Value::String(value) => value.replace('\n', "\\n"),
        value => serde_json::to_string(value).unwrap_or_else(|_| "<unrenderable JSON>".to_owned()),
    };

    truncate_chars(&formatted, MAX_FIELD_CHARS)
}

fn value_to_display_block(value: &Value, empty_summary: &str) -> DisplayBlock {
    let value = redact_sensitive(value);

    match value {
        Value::Object(object) if object.is_empty() => {
            DisplayBlock::Summary(empty_summary.to_owned())
        }
        Value::Object(object) => {
            let mut entries = object
                .into_iter()
                .map(|(key, value)| KeyValueEntry {
                    key,
                    value: format_field_value(&value),
                })
                .collect::<Vec<_>>();
            entries.sort_unstable_by(|left, right| left.key.cmp(&right.key));

            DisplayBlock::KeyValue { entries }
        }
        Value::String(content) => {
            let (content, truncated_lines) = truncate_preview(&content);
            DisplayBlock::TextOutput {
                content,
                truncated_lines,
            }
        }
        value => {
            let content = serde_json::to_string_pretty(&value)
                .unwrap_or_else(|_| "<unrenderable JSON>".to_owned());
            let (content, truncated_lines) = truncate_preview(&content);
            DisplayBlock::TextOutput {
                content,
                truncated_lines,
            }
        }
    }
}

fn default_presentation(
    call: &ToolCall,
    status: ToolCallStatus,
    blocks: Vec<DisplayBlock>,
) -> Presentation {
    Presentation {
        call_id: call.id.clone(),
        name: humanize_tool_name(&call.name),
        label: "tool".to_owned(),
        target: None,
        status,
        blocks,
    }
}

impl Presenter for DefaultPresenter {
    fn running(&self, call: &ToolCall) -> Presentation {
        default_presentation(
            call,
            ToolCallStatus::Running,
            vec![value_to_display_block(&call.arguments, "No arguments")],
        )
    }

    fn completed(&self, call: &ToolCall, result: &ToolCallResult) -> Presentation {
        match &result.outcome {
            ToolCallOutcome::Success(output) => default_presentation(
                call,
                ToolCallStatus::Succeeded,
                vec![value_to_display_block(output, "Completed")],
            ),
            ToolCallOutcome::Failure { message } => {
                let message = truncate_chars(message, MAX_ERROR_CHARS);
                default_presentation(
                    call,
                    ToolCallStatus::Failed { message },
                    vec![DisplayBlock::Summary("Tool execution failed".to_owned())],
                )
            }
        }
    }
}

pub struct WriteFilePresenter;

impl Presenter for WriteFilePresenter {
    fn completed(&self, call: &ToolCall, result: &ToolCallResult) -> Presentation {
        let path = call
            .arguments
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_owned);

        let content = call
            .arguments
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default();

        let lines_cnt = content.lines().count();

        let (status, blocks) = match &result.outcome {
            ToolCallOutcome::Success(_) => (
                ToolCallStatus::Succeeded,
                vec![
                    DisplayBlock::Summary(format!("Wrote {lines_cnt} lines")),
                    DisplayBlock::CodeBlock {
                        language: Some("raw".to_string()),
                        content: content.to_owned(),
                        truncated_lines: 10,
                    },
                ],
            ),
            ToolCallOutcome::Failure { message } => (
                ToolCallStatus::Failed {
                    message: message.clone(),
                },
                vec![DisplayBlock::Summary("Failed to write file".to_owned())],
            ),
        };

        Presentation {
            call_id: call.id.clone(),
            name: "Write".to_owned(),
            label: "built-in".to_owned(),
            target: path,
            status,
            blocks,
        }
    }

    fn running(&self, call: &ToolCall) -> Presentation {
        let path = call
            .arguments
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_owned);

        Presentation {
            call_id: call.id.clone(),
            name: "Write".to_owned(),
            label: "built-in".to_owned(),
            target: path,
            status: ToolCallStatus::Running,
            blocks: Vec::new(),
        }
    }
}

#[cfg(test)]
mod presenter_tests {
    use super::*;
    use serde_json::json;

    fn call(name: &str, arguments: Value) -> ToolCall {
        ToolCall {
            id: ToolCallId("call-1".to_owned()),
            name: name.to_owned(),
            arguments,
        }
    }

    #[test]
    fn humanizes_tool_names() {
        assert_eq!(humanize_tool_name("read_file"), "Read File");
        assert_eq!(humanize_tool_name("web-search"), "Web Search");
        assert_eq!(humanize_tool_name("GitHub_API"), "GitHub API");
        assert_eq!(humanize_tool_name("___"), "Tool");
    }

    #[test]
    fn running_presents_sorted_redacted_arguments() {
        let presentation = DefaultPresenter.running(&call(
            "custom_tool",
            json!({
                "zeta": 42,
                "api_key": "secret-value",
                "alpha": true,
            }),
        ));

        assert!(matches!(presentation.status, ToolCallStatus::Running));
        assert_eq!(presentation.name, "Custom Tool");
        assert_eq!(presentation.label, "tool");
        assert!(presentation.target.is_none());

        let DisplayBlock::KeyValue { entries } = &presentation.blocks[0] else {
            panic!("expected key-value arguments");
        };
        assert_eq!(entries[0].key, "alpha");
        assert_eq!(entries[1].key, "api_key");
        assert_eq!(entries[1].value, REDACTED);
        assert_eq!(entries[2].key, "zeta");
    }

    #[test]
    fn running_presents_non_object_arguments_as_text() {
        for arguments in [json!("hello"), json!([1, 2, 3]), json!(true)] {
            let presentation = DefaultPresenter.running(&call("tool", arguments));
            assert!(matches!(
                presentation.blocks[0],
                DisplayBlock::TextOutput { .. }
            ));
        }
    }

    #[test]
    fn completed_presents_successful_object_output() {
        let call = call("lookup", json!({}));
        let result = ToolCallResult {
            id: call.id.clone(),
            outcome: ToolCallOutcome::Success(json!({
                "status": "ok",
                "token": "must-not-leak",
            })),
        };
        let presentation = DefaultPresenter.completed(&call, &result);

        assert!(matches!(presentation.status, ToolCallStatus::Succeeded));
        let DisplayBlock::KeyValue { entries } = &presentation.blocks[0] else {
            panic!("expected key-value output");
        };
        assert_eq!(entries[0].key, "status");
        assert_eq!(entries[1].key, "token");
        assert_eq!(entries[1].value, REDACTED);
    }

    #[test]
    fn recursively_redacts_nested_sensitive_fields() {
        let presentation = DefaultPresenter.running(&call(
            "nested",
            json!({
                "config": {
                    "authorization": "Bearer secret",
                    "nested": [{ "password": "secret" }],
                }
            }),
        ));

        let DisplayBlock::KeyValue { entries } = &presentation.blocks[0] else {
            panic!("expected key-value arguments");
        };
        assert!(entries[0].value.contains(REDACTED));
        assert!(!entries[0].value.contains("Bearer secret"));
        assert!(!entries[0].value.contains("\"secret\""));
    }

    #[test]
    fn completed_presents_failure_with_truncated_message() {
        let call = call("failing_tool", json!({}));
        let result = ToolCallResult {
            id: call.id.clone(),
            outcome: ToolCallOutcome::Failure {
                message: "错误".repeat(MAX_ERROR_CHARS),
            },
        };
        let presentation = DefaultPresenter.completed(&call, &result);

        let ToolCallStatus::Failed { message } = presentation.status else {
            panic!("expected failed status");
        };
        assert!(message.ends_with("… [truncated]"));
        assert!(matches!(presentation.blocks[0], DisplayBlock::Summary(_)));
    }

    #[test]
    fn truncates_long_multiline_unicode_output_safely() {
        let content = (0..30)
            .map(|index| format!("第 {index} 行 {}", "界".repeat(300)))
            .collect::<Vec<_>>()
            .join("\n");
        let call = call("long_output", json!({}));
        let result = ToolCallResult {
            id: call.id.clone(),
            outcome: ToolCallOutcome::Success(Value::String(content)),
        };
        let presentation = DefaultPresenter.completed(&call, &result);

        let DisplayBlock::TextOutput {
            content,
            truncated_lines,
        } = &presentation.blocks[0]
        else {
            panic!("expected text output");
        };
        assert!(content.ends_with("… [truncated]"));
        assert!(*truncated_lines > 0);
        assert!(content.is_char_boundary(content.len()));
    }
}
