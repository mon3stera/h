use std::collections::BTreeMap;

use async_openai::{
    Client,
    config::OpenAIConfig,
    types::responses::{
        CreateResponseArgs, EasyInputMessage, FunctionTool, InputItem, ResponseStreamEvent,
        Tool as OpenAITool,
    },
};
use futures::{StreamExt, TryStreamExt};
use parking_lot::Mutex;
use serde_json::{Map, Value};

use crate::{
    context::Context,
    event::ProviderSignal,
    provider::{Provider, ProviderEventStream, TurnStart, UserMessage},
    tool::{ToolDefinition, ToolResult},
};

macro_rules! expect_env {
    ($value:expr) => {
        std::env::var($value)?
    };
}

pub struct OpenAIProviderConfig {
    base_url: String,
    api_key: String,
    model: String,
}

impl OpenAIProviderConfig {
    pub fn new() -> Self {
        Self {
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
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

    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            base_url: expect_env!("OPENAI_BASE_URL"),
            api_key: expect_env!("OPENAI_API_KEY"),
            model: expect_env!("OPENAI_MODEL"),
        })
    }
}

struct PendingItem {
    item: InputItem,
}

pub struct OpenAIProvider {
    config: OpenAIProviderConfig,
    client: Client<OpenAIConfig>,
    context: Context<InputItem>,
    tools: Vec<OpenAITool>,
    prompt: Mutex<String>,
    pending: BTreeMap<u32, PendingItem>,
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
            context: Context::new(),
            tools: Vec::new(),
            prompt: Mutex::new(String::new()),
            pending: BTreeMap::new(),
        }
    }

    fn sanitize_schema(schema: &Value) -> Value {
        let mut schema = schema.clone();
        Self::sanitize_schema_node(&mut schema);
        schema
    }

    fn sanitize_schema_node(schema: &mut Value) {
        let Value::Object(object) = schema else {
            return;
        };

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
            Self::sanitize_object_schema(object);
        }

        if let Some(items) = object.get_mut("items") {
            Self::sanitize_schema_node(items);
        }

        for keyword in ["anyOf", "oneOf", "allOf"] {
            if let Some(Value::Array(branches)) = object.get_mut(keyword) {
                for branch in branches {
                    Self::sanitize_schema_node(branch);
                }
            }
        }

        for keyword in ["$defs", "definitions"] {
            if let Some(Value::Object(definitions)) = object.get_mut(keyword) {
                for definition in definitions.values_mut() {
                    Self::sanitize_schema_node(definition);
                }
            }
        }
    }

    fn sanitize_object_schema(schema: &mut Map<String, Value>) {
        let required = schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<std::collections::BTreeSet<_>>();

        let property_names = match schema.get_mut("properties") {
            Some(Value::Object(properties)) => {
                let property_names = properties.keys().cloned().collect::<Vec<_>>();

                for (name, property_schema) in properties {
                    Self::sanitize_schema_node(property_schema);

                    if !required.contains(name) {
                        Self::make_nullable(property_schema);
                    }
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

    fn make_nullable(schema: &mut Value) {
        if Self::is_nullable(schema) {
            return;
        }

        if let Value::Object(object) = schema {
            match object.get_mut("type") {
                Some(Value::String(kind)) => {
                    let kind = std::mem::take(kind);
                    object.insert(
                        "type".to_owned(),
                        Value::Array(vec![Value::String(kind), Value::String("null".to_owned())]),
                    );
                    return;
                }
                Some(Value::Array(types)) => {
                    types.push(Value::String("null".to_owned()));
                    return;
                }
                _ => {}
            }
        }

        let original = std::mem::take(schema);
        *schema = serde_json::json!({
            "anyOf": [
                original,
                { "type": "null" }
            ]
        });
    }

    fn is_nullable(schema: &Value) -> bool {
        let Some(object) = schema.as_object() else {
            return false;
        };

        match object.get("type") {
            Some(Value::String(kind)) if kind == "null" => return true,
            Some(Value::Array(types)) if types.iter().any(|kind| kind.as_str() == Some("null")) => {
                return true;
            }
            _ => {}
        }

        object
            .get("anyOf")
            .and_then(Value::as_array)
            .is_some_and(|branches| branches.iter().any(Self::is_null_schema))
    }

    fn is_null_schema(schema: &Value) -> bool {
        schema
            .as_object()
            .and_then(|object| object.get("type"))
            .is_some_and(|kind| kind.as_str() == Some("null"))
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

    fn take_pending(&mut self) -> (String, BTreeMap<u32, PendingItem>) {
        let mut prompt = String::new();
        let mut pending = BTreeMap::new();

        std::mem::swap(&mut prompt, &mut self.prompt.lock());
        std::mem::swap(&mut pending, &mut self.pending);

        (prompt, pending)
    }

    fn build_input(&self, messages: &UserMessage) -> Vec<InputItem> {
        let mut inputs = Vec::<InputItem>::new();

        inputs.extend(self.context.histories().iter().cloned());

        for message in &messages.contents {
            match message {
                crate::provider::Message::Text(text) => {
                    inputs.push(EasyInputMessage::from(text.as_str()).into());
                }
            }
        }

        inputs
    }

    fn record_history(&mut self, event: &ResponseStreamEvent) -> anyhow::Result<()> {
        match event {
            ResponseStreamEvent::ResponseOutputItemDone(item) => {
                self.pending.insert(
                    item.output_index,
                    PendingItem {
                        item: item.item.clone().into(),
                    },
                );
            }
            ResponseStreamEvent::ResponseCompleted(_) => {
                let (prompt, pending) = self.take_pending();

                let items = pending.into_values().map(|e| e.item);

                let histories = self.context.histories_mut();

                if !prompt.is_empty() {
                    histories.push(EasyInputMessage::from(prompt).into());
                }

                histories.extend(items);
            }
            _ => {}
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl Provider for OpenAIProvider {
    type StreamEvent = async_openai::types::responses::ResponseStreamEvent;

    fn define_tools(&mut self, specs: Vec<ToolDefinition>) -> anyhow::Result<()> {
        self.tools = specs.into_iter().map(Self::compile_tool).collect();
        Ok(())
    }

    fn submit_tool_result(&mut self, _result: ToolResult) -> anyhow::Result<()> {
        todo!("store the OpenAI function call output in the provider context")
    }

    async fn handle(&mut self, event: Self::StreamEvent) -> anyhow::Result<ProviderSignal> {
        self.record_history(&event)?;

        match &event {
            ResponseStreamEvent::ResponseOutputTextDelta(delta) => {
                return Ok(ProviderSignal::TextDelta(delta.delta.clone()));
            }
            ResponseStreamEvent::ResponseCompleted(_) => Ok(ProviderSignal::Completed),
            _ => Ok(ProviderSignal::Unsupported),
        }
    }

    async fn stream(
        &self,
        start: TurnStart,
    ) -> anyhow::Result<ProviderEventStream<Self::StreamEvent>> {
        let (input, prompt) = match start {
            TurnStart::UserMessage(message) => {
                let prompt = message
                    .contents
                    .iter()
                    .map(|content| match content {
                        crate::provider::Message::Text(text) => text.as_str(),
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                (self.build_input(&message), prompt)
            }
            TurnStart::Continue => (self.context.histories().to_vec(), String::new()),
        };

        let request = CreateResponseArgs::default()
            .model(&self.config.model)
            .input(input)
            .tools(self.tools.clone())
            .stream(true)
            .build()?;

        let stream = self
            .client
            .responses()
            .create_stream(request)
            .await?
            .map_err(anyhow::Error::from)
            .boxed();

        *self.prompt.lock() = prompt;

        Ok(stream)
    }
}

#[cfg(test)]
mod tests {
    use super::OpenAIProvider;
    use serde_json::json;

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
                    "$ref": "#/$defs/SearchOptions"
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
                        "anyOf": [
                            { "$ref": "#/$defs/SearchOptions" },
                            { "type": "null" }
                        ]
                    }
                },
                "required": ["limit", "options", "query"],
                "additionalProperties": false,
                "$defs": {
                    "SearchOptions": {
                        "type": "object",
                        "properties": {
                            "include_archived": {
                                "type": "boolean"
                            }
                        },
                        "required": ["include_archived"],
                        "additionalProperties": false
                    }
                }
            })
        );
    }
}
