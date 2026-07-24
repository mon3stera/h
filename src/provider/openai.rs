use std::collections::BTreeMap;

use async_openai::{
    Client,
    config::OpenAIConfig,
    error::OpenAIError,
    types::responses::{
        CreateResponse, CreateResponseArgs, EasyInputContent, EasyInputMessage, FunctionCallOutput,
        FunctionCallOutputItemParam, FunctionTool, FunctionToolCall,
        FunctionToolCallOutputResource,
        InputItem::{self, EasyMessage},
        Item, MessageType, OutputItem, OutputMessageContent, OutputStatus, Reasoning,
        ReasoningEffort, ResponseStreamEvent, Role, Tool as OpenAITool,
    },
};
use futures::{StreamExt, TryStreamExt};
use parking_lot::Mutex;
use serde_json::{Map, Value};

use crate::{
    context::{Context, Message},
    event::{CompletedReason, ProviderSignal},
    provider::{Provider, ProviderEventStream},
    tool::{ToolCall, ToolCallResult, ToolDefinition},
};

macro_rules! expect_env {
    ($value:expr) => {
        std::env::var($value)?
    };
}

const REASONING_EFFORT_VALUES: &str = "none, minimal, low, medium, high, xhigh";

fn parse_reasoning_effort(value: &str) -> anyhow::Result<ReasoningEffort> {
    match value {
        "none" => Ok(ReasoningEffort::None),
        "minimal" => Ok(ReasoningEffort::Minimal),
        "low" => Ok(ReasoningEffort::Low),
        "medium" => Ok(ReasoningEffort::Medium),
        "high" => Ok(ReasoningEffort::High),
        "xhigh" => Ok(ReasoningEffort::Xhigh),
        _ => anyhow::bail!(
            "invalid OPENAI_REASONING_EFFORT {value:?}; expected one of: {REASONING_EFFORT_VALUES}"
        ),
    }
}

fn reasoning_effort_from_env() -> anyhow::Result<Option<ReasoningEffort>> {
    match std::env::var("OPENAI_REASONING_EFFORT") {
        Ok(value) => parse_reasoning_effort(&value).map(Some),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(anyhow::anyhow!("OPENAI_REASONING_EFFORT: {error}")),
    }
}

fn reasoning_effort_name(effort: &ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::None => "none",
        ReasoningEffort::Minimal => "minimal",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::Xhigh => "xhigh",
    }
}

fn is_ignorable_stream_event(error: &OpenAIError) -> bool {
    let OpenAIError::JSONDeserialize(_, raw) = error else {
        return false;
    };

    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|value| value.get("type").and_then(Value::as_str).map(str::to_owned))
        .is_some_and(|event_type| event_type == "response.metadata")
}

pub struct OpenAIProviderConfig {
    base_url: String,
    api_key: String,
    model: String,
    reasoning_effort: Option<ReasoningEffort>,
}

impl OpenAIProviderConfig {
    pub fn new() -> Self {
        Self {
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            reasoning_effort: None,
        }
    }

    pub fn with_base_url(self, base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            ..self
        }
    }

    pub fn with_api_key(self, api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            ..self
        }
    }

    pub fn with_model(self, model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            ..self
        }
    }

    pub fn with_reasoning_effort(self, reasoning_effort: ReasoningEffort) -> Self {
        Self {
            reasoning_effort: Some(reasoning_effort),
            ..self
        }
    }

    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            base_url: expect_env!("OPENAI_BASE_URL"),
            api_key: expect_env!("OPENAI_API_KEY"),
            model: expect_env!("OPENAI_MODEL"),
            reasoning_effort: reasoning_effort_from_env()?,
        })
    }
}

pub struct OpenAIProvider {
    config: OpenAIProviderConfig,
    client: Client<OpenAIConfig>,
    tools: Vec<OpenAITool>,
}

impl OpenAIProvider {
    pub fn from_config(config: OpenAIProviderConfig) -> Self {
        let client_config = OpenAIConfig::new()
            .with_api_base(&config.base_url)
            .with_api_key(&config.api_key);

        let client = Client::with_config(client_config);

        Self {
            config,
            client,
            tools: Vec::new(),
        }
    }

    fn build_request(&self, input: Vec<InputItem>) -> anyhow::Result<CreateResponse> {
        let mut request = CreateResponseArgs::default();
        request
            .model(&self.config.model)
            .input(input)
            .tools(self.tools.clone())
            .stream(true);

        if let Some(effort) = self.config.reasoning_effort.clone() {
            request.reasoning(Reasoning::from(effort));
        }

        Ok(request.build()?)
    }

    fn sanitize_schema(schema: &Value) -> Value {
        let mut schema = schema.clone();
        let definitions = schema
            .as_object()
            .and_then(|object| object.get("$defs").or_else(|| object.get("definitions")))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();

        Self::sanitize_schema_node(&mut schema, &definitions);

        if let Value::Object(object) = &mut schema {
            object.remove("$defs");
            object.remove("definitions");
        }

        schema
    }

    fn sanitize_schema_node(schema: &mut Value, definitions: &Map<String, Value>) {
        let Value::Object(object) = schema else {
            return;
        };

        if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
            if let Some(name) = reference
                .strip_prefix("#/$defs/")
                .or_else(|| reference.strip_prefix("#/definitions/"))
            {
                if let Some(definition) = definitions.get(name) {
                    let annotations = std::mem::take(object);
                    let mut resolved = definition.clone();
                    Self::sanitize_schema_node(&mut resolved, definitions);

                    let Value::Object(resolved) = resolved else {
                        return;
                    };

                    *object = resolved;
                    for (keyword, value) in annotations {
                        if keyword != "$ref" {
                            object.insert(keyword, value);
                        }
                    }
                }
            }
        }

        for keyword in [
            "$schema",
            "$id",
            "title",
            "examples",
            "deprecated",
            "readOnly",
            "writeOnly",
            "minimum",
            "maximum",
            "exclusiveMinimum",
            "exclusiveMaximum",
            "multipleOf",
            "minLength",
            "maxLength",
            "pattern",
            "maxItems",
            "uniqueItems",
            "contains",
            "minContains",
            "maxContains",
            "minProperties",
            "maxProperties",
            "patternProperties",
            "propertyNames",
            "dependentRequired",
            "dependentSchemas",
            "if",
            "then",
            "else",
            "not",
            "unevaluatedProperties",
            "unevaluatedItems",
        ] {
            object.remove(keyword);
        }

        Self::sanitize_format(object);

        let is_object = Self::has_type(object, "object") || object.contains_key("properties");
        if is_object {
            Self::sanitize_object_schema(object, definitions);
        }

        if let Some(items) = object.get_mut("items") {
            Self::sanitize_schema_node(items, definitions);
        }

        for keyword in ["anyOf", "oneOf", "allOf"] {
            if let Some(Value::Array(branches)) = object.get_mut(keyword) {
                for branch in branches {
                    Self::sanitize_schema_node(branch, definitions);
                }
            }
        }
    }

    fn sanitize_object_schema(schema: &mut Map<String, Value>, definitions: &Map<String, Value>) {
        let property_names = match schema.get_mut("properties") {
            Some(Value::Object(properties)) => {
                let property_names = properties.keys().cloned().collect::<Vec<_>>();

                for property_schema in properties.values_mut() {
                    Self::sanitize_schema_node(property_schema, definitions);
                }

                property_names
            }
            _ => Vec::new(),
        };

        schema.insert("additionalProperties".to_owned(), Value::Bool(false));
        schema.insert(
            "required".to_owned(),
            Value::Array(property_names.into_iter().map(Value::String).collect()),
        );
    }

    fn has_type(schema: &Map<String, Value>, expected: &str) -> bool {
        match schema.get("type") {
            Some(Value::String(kind)) => kind == expected,
            Some(Value::Array(types)) => types.iter().any(|kind| kind.as_str() == Some(expected)),
            _ => false,
        }
    }

    fn sanitize_format(schema: &mut Map<String, Value>) {
        const SUPPORTED_FORMATS: &[&str] = &[
            "date-time",
            "time",
            "date",
            "duration",
            "email",
            "hostname",
            "uri",
            "ipv4",
            "ipv6",
            "uuid",
        ];

        let supported = schema
            .get("format")
            .and_then(Value::as_str)
            .is_some_and(|format| SUPPORTED_FORMATS.contains(&format));

        if schema.contains_key("format") && !supported {
            schema.remove("format");
        }
    }

    fn compile_tool(spec: ToolDefinition) -> OpenAITool {
        FunctionTool {
            name: spec.name,
            parameters: Some(Self::sanitize_schema(&spec.arguments)),
            strict: Some(true),
            description: Some(spec.description),
            defer_loading: Some(false),
        }
        .into()
    }
}

impl From<Message> for InputItem {
    fn from(value: Message) -> Self {
        match value {
            Message::User(text) => EasyInputMessage::from(text).into(),
            Message::System(text) => EasyInputMessage {
                r#type: MessageType::Message,
                role: Role::System,
                content: EasyInputContent::Text(text),
                phase: None,
            }
            .into(),
            Message::Assistant(text) => EasyInputMessage {
                r#type: MessageType::Message,
                role: Role::Assistant,
                content: EasyInputContent::Text(text),
                phase: None,
            }
            .into(),
            Message::ToolCall {
                call_id,
                arguments,
                name,
            } => InputItem::Item(Item::FunctionCall(FunctionToolCall {
                call_id: call_id,
                arguments: arguments,
                namespace: None,
                name: name,
                id: None,
                status: Some(OutputStatus::Completed),
            })),
            Message::ToolCallResult { call_id, output } => {
                InputItem::Item(Item::FunctionCallOutput(FunctionCallOutputItemParam {
                    call_id: call_id,
                    output: FunctionCallOutput::Text(output),
                    id: None,
                    status: Some(OutputStatus::Completed),
                }))
            }
        }
    }
}

impl TryFrom<OutputItem> for Message {
    type Error = anyhow::Error;

    fn try_from(value: OutputItem) -> Result<Self, Self::Error> {
        match value {
            OutputItem::Message(message) => {
                let text = message
                    .content
                    .into_iter()
                    .map(|o| match o {
                        OutputMessageContent::OutputText(text) => text.text,
                        OutputMessageContent::Refusal(text) => text.refusal,
                    })
                    .collect::<Vec<_>>()
                    .join("");

                Ok(Message::Assistant(text))
            }
            OutputItem::FunctionCall(FunctionToolCall {
                arguments,
                call_id,
                name,
                ..
            }) => Ok(Message::ToolCall {
                call_id,
                name,
                arguments,
            }),
            OutputItem::FunctionCallOutput(FunctionToolCallOutputResource {
                call_id,
                output,
                ..
            }) => {
                let text = match output {
                    FunctionCallOutput::Text(text) => text,
                    FunctionCallOutput::Content(_) => todo!(),
                };

                Ok(Message::ToolCallResult {
                    call_id,
                    output: text,
                })
            }
            _ => anyhow::bail!("Unsupported Message"),
        }
    }
}

#[async_trait::async_trait]
impl Provider for OpenAIProvider {
    type StreamEvent = async_openai::types::responses::ResponseStreamEvent;

    fn model(&self) -> &str {
        &self.config.model
    }

    fn thinking_effort(&self) -> Option<&str> {
        self.config
            .reasoning_effort
            .as_ref()
            .map(reasoning_effort_name)
    }

    fn define_tools(&mut self, specs: Vec<ToolDefinition>) -> anyhow::Result<()> {
        let tool_count = specs.len();
        self.tools = specs.into_iter().map(Self::compile_tool).collect();
        tracing::info!(
            event = "provider.tools.defined",
            provider = "openai",
            tool_count
        );
        Ok(())
    }

    async fn handle(&mut self, event: Self::StreamEvent) -> anyhow::Result<ProviderSignal> {
        match &event {
            /* ResponseStreamEvent::ResponseFunctionCallArgumentsDelta(delta) => {
                delta.
            } */
            ResponseStreamEvent::ResponseOutputItemDone(done) => match &done.item {
                OutputItem::FunctionCall(call) => {
                    tracing::info!(
                        event = "provider.tool_call.requested",
                        provider = "openai",
                        tool_name = call.name
                    );
                    let arguments = serde_json::from_str(&call.arguments)?;

                    Ok(ProviderSignal::ToolCallStarted(ToolCall::new(
                        call.call_id.clone(),
                        call.name.clone(),
                        arguments,
                    )))
                }
                OutputItem::FunctionCallOutput(call) => {
                    let output = match &call.output {
                        FunctionCallOutput::Text(text) => serde_json::from_str(text)
                            .unwrap_or_else(|_| Value::String(text.clone())),
                        FunctionCallOutput::Content(content) => serde_json::to_value(content)?,
                    };

                    Ok(ProviderSignal::ToolCallCompleted(ToolCallResult::success(
                        call.call_id.clone(),
                        output,
                    )))
                }
                _ => Ok(ProviderSignal::Unsupported),
            },
            ResponseStreamEvent::ResponseOutputTextDelta(delta) => {
                return Ok(ProviderSignal::TextDelta(delta.delta.clone()));
            }
            ResponseStreamEvent::ResponseCompleted(completed) => {
                let need_call = completed
                    .response
                    .output
                    .iter()
                    .any(|e| matches!(e, OutputItem::FunctionCall(_)));

                tracing::info!(
                    event = "provider.response.completed",
                    provider = "openai",
                    completion_reason = if need_call {
                        "needs_tool_call"
                    } else {
                        "final"
                    }
                );

                Ok(ProviderSignal::Completed(if need_call {
                    CompletedReason::NeedCall
                } else {
                    CompletedReason::Final
                }))
            }
            _ => Ok(ProviderSignal::Unsupported),
        }
    }

    async fn stream(
        &self,
        messages: &[Message],
    ) -> anyhow::Result<ProviderEventStream<Self::StreamEvent>> {
        let message_count = messages.len();
        let tool_count = self.tools.len();
        let input = messages
            .iter()
            .cloned()
            .map(|e| e.into())
            .collect::<Vec<InputItem>>();

        let request = self.build_request(input)?;

        let stream = self
            .client
            .responses()
            .create_stream(request)
            .await?
            .filter_map(|result| async move {
                match result {
                    Ok(event) => Some(Ok(event)),
                    Err(err) if is_ignorable_stream_event(&err) => None,
                    Err(error) => Some(Err(anyhow::Error::from(error))),
                }
            })
            .boxed();

        tracing::info!(
            event = "provider.stream.opened",
            provider = "openai",
            message_count,
            tool_count
        );

        Ok(stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{provider::Provider, tool::WriteFileToolArgs};
    use serde_json::json;

    fn provider(reasoning_effort: Option<ReasoningEffort>) -> OpenAIProvider {
        let mut config = OpenAIProviderConfig::new()
            .with_base_url("https://example.com")
            .with_api_key("secret")
            .with_model("gpt-5.6-sol");
        if let Some(reasoning_effort) = reasoning_effort {
            config = config.with_reasoning_effort(reasoning_effort);
        }

        OpenAIProvider::from_config(config)
    }

    #[test]
    fn parses_supported_reasoning_efforts() {
        for (name, expected) in [
            ("none", ReasoningEffort::None),
            ("minimal", ReasoningEffort::Minimal),
            ("low", ReasoningEffort::Low),
            ("medium", ReasoningEffort::Medium),
            ("high", ReasoningEffort::High),
            ("xhigh", ReasoningEffort::Xhigh),
        ] {
            assert_eq!(parse_reasoning_effort(name).unwrap(), expected);
            assert_eq!(reasoning_effort_name(&expected), name);
        }
    }

    #[test]
    fn rejects_invalid_reasoning_efforts() {
        for value in ["", "max", "HIGH", "unknown"] {
            let error = parse_reasoning_effort(value).unwrap_err().to_string();
            assert!(error.contains("OPENAI_REASONING_EFFORT"));
            assert!(error.contains(REASONING_EFFORT_VALUES));
        }
    }

    #[test]
    fn request_and_accessors_use_configured_reasoning_effort() {
        let provider = provider(Some(ReasoningEffort::High));
        let request = provider.build_request(Vec::new()).unwrap();
        let value = serde_json::to_value(request).unwrap();

        assert_eq!(provider.model(), "gpt-5.6-sol");
        assert_eq!(provider.thinking_effort(), Some("high"));
        assert_eq!(value["model"], "gpt-5.6-sol");
        assert_eq!(value["reasoning"]["effort"], "high");
        assert!(!value.to_string().contains("secret"));
    }

    #[test]
    fn request_omits_reasoning_when_effort_is_not_configured() {
        let provider = provider(None);
        let request = provider.build_request(Vec::new()).unwrap();
        let value = serde_json::to_value(request).unwrap();

        assert_eq!(provider.thinking_effort(), None);
        assert!(value.get("reasoning").is_none());
    }

    #[test]
    fn sanitizes_schemars_schema_for_openai_strict_tools() {
        let schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "SearchArguments",
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "minLength": 1
                },
                "limit": {
                    "type": ["integer", "null"],
                    "format": "uint32",
                    "minimum": 0
                },
                "options": {
                    "$ref": "#/$defs/SearchOptions",
                    "description": "Search options."
                }
            },
            "required": ["query"],
            "$defs": {
                "SearchOptions": {
                    "type": "object",
                    "properties": {
                        "include_archived": {
                            "type": "boolean"
                        }
                    },
                    "required": ["include_archived"]
                }
            }
        });

        let sanitized = OpenAIProvider::sanitize_schema(&schema);

        assert_eq!(
            sanitized,
            json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string"
                    },
                    "limit": {
                        "type": ["integer", "null"]
                    },
                    "options": {
                        "type": "object",
                        "description": "Search options.",
                        "properties": {
                            "include_archived": {
                                "type": "boolean"
                            }
                        },
                        "required": ["include_archived"],
                        "additionalProperties": false
                    }
                },
                "required": ["limit", "options", "query"],
                "additionalProperties": false
            })
        );
    }

    #[test]
    fn sanitizes_write_file_schema_for_openai_strict_tools() {
        let schema = serde_json::to_value(schemars::schema_for!(WriteFileToolArgs)).unwrap();
        let sanitized = OpenAIProvider::sanitize_schema(&schema);

        assert_eq!(
            sanitized,
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path."
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["overwrite", "append"],
                        "description": "Write mode. `overwrite` replaces the file; `append` adds content to the end. Defaults to `overwrite`."
                    }
                },
                "required": ["content", "mode", "path"],
                "additionalProperties": false
            })
        );
    }
}
