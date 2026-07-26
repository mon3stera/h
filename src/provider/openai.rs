use std::collections::{BTreeMap, BTreeSet};

use async_openai::{
    Client,
    config::OpenAIConfig,
    error::OpenAIError,
    types::responses::{
        CodeInterpreterTool, CreateResponse, CreateResponseArgs, EasyInputContent,
        EasyInputMessage, FileSearchTool, FunctionCallOutput, FunctionCallOutputItemParam,
        FunctionTool, FunctionToolCall, FunctionToolCallOutputResource,
        InputItem::{self, EasyMessage},
        Item, MessageType, OutputItem, OutputMessageContent, OutputStatus, Reasoning,
        ReasoningEffort, ResponseStreamEvent, Role, Tool as OpenAITool, WebSearchTool,
    },
};
use futures::{StreamExt, TryStreamExt};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};

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

        let tools = vec![async_openai::types::responses::Tool::WebSearch(
            WebSearchTool::default(),
        )];

        Self {
            config,
            client,
            tools,
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

    fn sanitize_schema(schema: &Value) -> anyhow::Result<Value> {
        let mut schema = schema.clone();
        let definitions = schema
            .as_object()
            .and_then(|object| object.get("$defs").or_else(|| object.get("definitions")))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();

        Self::sanitize_schema_node(&mut schema, &definitions)?;

        if let Value::Object(object) = &mut schema {
            object.remove("$defs");
            object.remove("definitions");
        }

        Ok(schema)
    }

    fn sanitize_schema_node(
        schema: &mut Value,
        definitions: &Map<String, Value>,
    ) -> anyhow::Result<()> {
        let Value::Object(object) = schema else {
            return Ok(());
        };

        if let Some(reference) = object
            .get("$ref")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            if let Some(name) = reference
                .strip_prefix("#/$defs/")
                .or_else(|| reference.strip_prefix("#/definitions/"))
            {
                if let Some(definition) = definitions.get(name) {
                    let annotations = std::mem::take(object);
                    let mut resolved = definition.clone();
                    Self::sanitize_schema_node(&mut resolved, definitions)?;

                    let Value::Object(resolved) = resolved else {
                        anyhow::bail!(
                            "OpenAI tool schema reference {reference:?} did not resolve to an object"
                        );
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

        if object.contains_key("oneOf") {
            Self::lower_one_of(object, definitions)?;
        }

        let is_object = Self::has_type(object, "object") || object.contains_key("properties");
        if is_object {
            Self::sanitize_object_schema(object, definitions)?;
        }

        if let Some(items) = object.get_mut("items") {
            Self::sanitize_schema_node(items, definitions)?;
        }

        for keyword in ["anyOf", "allOf"] {
            if let Some(Value::Array(branches)) = object.get_mut(keyword) {
                for branch in branches {
                    Self::sanitize_schema_node(branch, definitions)?;
                }
            }
        }

        Ok(())
    }

    fn sanitize_object_schema(
        schema: &mut Map<String, Value>,
        definitions: &Map<String, Value>,
    ) -> anyhow::Result<()> {
        let property_names = match schema.get_mut("properties") {
            Some(Value::Object(properties)) => {
                let property_names = properties.keys().cloned().collect::<Vec<_>>();

                for property_schema in properties.values_mut() {
                    Self::sanitize_schema_node(property_schema, definitions)?;
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
        Ok(())
    }

    fn lower_one_of(
        schema: &mut Map<String, Value>,
        definitions: &Map<String, Value>,
    ) -> anyhow::Result<()> {
        let Some(Value::Array(branches)) = schema.remove("oneOf") else {
            return Ok(());
        };

        if Self::lower_const_enum(schema, &branches)? {
            return Ok(());
        }

        if Self::lower_tagged_object_union(schema, &branches, definitions)? {
            return Ok(());
        }

        anyhow::bail!(
            "OpenAI strict tool schema does not support oneOf unless it is a documented enum or internally tagged object union"
        );
    }

    fn is_string_or_untyped(schema: &Map<String, Value>) -> bool {
        match schema.get("type") {
            None => true,
            Some(Value::String(kind)) => kind == "string",
            _ => false,
        }
    }

    fn lower_const_enum(
        schema: &mut Map<String, Value>,
        branches: &[Value],
    ) -> anyhow::Result<bool> {
        if branches.is_empty() {
            return Ok(false);
        }

        let mut values = Vec::with_capacity(branches.len());
        let mut seen = BTreeSet::new();
        let mut descriptions = Vec::new();

        for branch in branches {
            let Value::Object(branch) = branch else {
                return Ok(false);
            };
            let Some(value) = branch.get("const").and_then(Value::as_str) else {
                return Ok(false);
            };
            if !Self::is_string_or_untyped(branch)
                || branch
                    .keys()
                    .any(|key| !matches!(key.as_str(), "const" | "type" | "description" | "title"))
            {
                return Ok(false);
            }
            if !seen.insert(value.to_owned()) {
                anyhow::bail!("OpenAI strict tool schema enum repeats oneOf value {value:?}");
            }

            values.push(Value::String(value.to_owned()));
            if let Some(description) = branch.get("description").and_then(Value::as_str) {
                descriptions.push(format!("- `{value}`: {description}"));
            }
        }

        schema.insert("type".to_owned(), Value::String("string".to_owned()));
        schema.insert("enum".to_owned(), Value::Array(values));
        Self::append_description(schema, "Allowed values", descriptions);
        Ok(true)
    }

    fn lower_tagged_object_union(
        schema: &mut Map<String, Value>,
        branches: &[Value],
        definitions: &Map<String, Value>,
    ) -> anyhow::Result<bool> {
        if branches.is_empty() {
            return Ok(false);
        }

        let mut discriminator_candidates: Option<BTreeSet<String>> = None;
        for branch in branches {
            let Value::Object(branch) = branch else {
                return Ok(false);
            };
            if !Self::has_type(branch, "object")
                || branch.keys().any(|key| {
                    !matches!(
                        key.as_str(),
                        "type"
                            | "properties"
                            | "required"
                            | "description"
                            | "title"
                            | "additionalProperties"
                    )
                })
                || !matches!(
                    branch.get("additionalProperties"),
                    None | Some(Value::Bool(false))
                )
            {
                return Ok(false);
            }
            let Some(Value::Object(properties)) = branch.get("properties") else {
                return Ok(false);
            };
            let required = Self::required_property_names(branch);
            let candidates = properties
                .iter()
                .filter_map(|(name, property)| {
                    let property = property.as_object()?;
                    let value = property.get("const")?.as_str()?;
                    (required.contains(name)
                        && Self::is_string_or_untyped(property)
                        && !value.is_empty())
                    .then(|| name.clone())
                })
                .collect::<BTreeSet<_>>();

            discriminator_candidates = Some(match discriminator_candidates {
                Some(candidates_so_far) => candidates_so_far
                    .intersection(&candidates)
                    .cloned()
                    .collect(),
                None => candidates,
            });
        }

        let Some(discriminator) = discriminator_candidates.and_then(|mut candidates| {
            (candidates.len() == 1).then(|| candidates.pop_first().expect("one candidate"))
        }) else {
            return Ok(false);
        };

        let mut action_values = Vec::with_capacity(branches.len());
        let mut action_descriptions = Vec::with_capacity(branches.len());
        let mut properties = BTreeMap::<String, Value>::new();
        let mut property_count = BTreeMap::<String, usize>::new();
        let mut nullable_properties = BTreeSet::new();
        let mut property_descriptions = BTreeMap::<String, BTreeSet<(String, String)>>::new();

        for branch in branches {
            let Value::Object(branch) = branch else {
                unreachable!("validated above");
            };
            let Value::Object(branch_properties) =
                branch.get("properties").expect("validated above")
            else {
                unreachable!("validated above");
            };
            let required = Self::required_property_names(branch);
            let action = branch_properties
                .get(&discriminator)
                .and_then(Value::as_object)
                .and_then(|property| property.get("const"))
                .and_then(Value::as_str)
                .expect("validated discriminator")
                .to_owned();

            if action_values.iter().any(|value| value == &action) {
                anyhow::bail!(
                    "OpenAI strict tool schema tagged union repeats discriminator value {action:?}"
                );
            }

            let variant_description = branch
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("No description provided.");
            let required_for_action = required
                .iter()
                .filter(|field| field.as_str() != discriminator)
                .cloned()
                .collect::<Vec<_>>();
            let required_summary = if required_for_action.is_empty() {
                "No additional fields are required.".to_owned()
            } else {
                format!(
                    "Required fields: {}.",
                    required_for_action
                        .iter()
                        .map(|field| format!("`{field}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            action_descriptions.push(format!(
                "- `{action}`: {variant_description} {required_summary}"
            ));
            action_values.push(action.clone());

            for (name, property) in branch_properties {
                if name == &discriminator {
                    continue;
                }

                let mut property = property.clone();
                Self::sanitize_schema_node(&mut property, definitions)?;
                let (normalized_property, nullable) = Self::normalized_non_null_schema(&property)?;
                if let Some(existing) = properties.get(name) {
                    if !Self::schemas_match_ignoring_descriptions(existing, &normalized_property) {
                        anyhow::bail!(
                            "OpenAI strict tool schema cannot flatten tagged union property {name:?} because variants use incompatible schemas"
                        );
                    }
                } else {
                    properties.insert(name.clone(), normalized_property);
                }

                *property_count.entry(name.clone()).or_default() += 1;
                if nullable || !required.contains(name) {
                    nullable_properties.insert(name.clone());
                }
                if let Some(description) = property
                    .as_object()
                    .and_then(|property| property.get("description"))
                    .and_then(Value::as_str)
                {
                    property_descriptions
                        .entry(name.clone())
                        .or_default()
                        .insert((action.clone(), description.to_owned()));
                }
            }
        }

        for (name, count) in &property_count {
            if *count != branches.len() {
                nullable_properties.insert(name.clone());
            }
        }
        for name in nullable_properties {
            let property = properties
                .get_mut(&name)
                .expect("every nullable property was recorded");
            Self::make_nullable(property)?;
        }

        let action_schema = serde_json::json!({
            "type": "string",
            "enum": action_values,
        });
        let mut flattened_properties = Map::new();
        flattened_properties.insert(discriminator.clone(), action_schema);
        for (name, property) in properties {
            flattened_properties.insert(name, property);
        }

        schema.insert("type".to_owned(), Value::String("object".to_owned()));
        schema.insert("properties".to_owned(), Value::Object(flattened_properties));
        schema.remove("required");
        schema.remove("additionalProperties");

        Self::append_description(schema, "Available actions", action_descriptions);
        let mut varying_field_descriptions = Vec::new();
        for (field, descriptions) in property_descriptions {
            let unique_descriptions = descriptions
                .iter()
                .map(|(_, description)| description)
                .collect::<BTreeSet<_>>();
            if unique_descriptions.len() > 1 {
                for (action, description) in descriptions {
                    varying_field_descriptions
                        .push(format!("- `{action}` / `{field}`: {description}"));
                }
            }
        }
        Self::append_description(
            schema,
            "Field descriptions that vary by action",
            varying_field_descriptions,
        );
        Self::append_description(
            schema,
            "Flattened action input",
            vec![
                "All listed fields must be present. Set fields unused by the selected action to `null`."
                    .to_owned(),
            ],
        );

        Ok(true)
    }

    fn required_property_names(schema: &Map<String, Value>) -> BTreeSet<String> {
        schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    }

    fn normalized_non_null_schema(schema: &Value) -> anyhow::Result<(Value, bool)> {
        let mut schema = schema.clone();
        let Value::Object(object) = &mut schema else {
            anyhow::bail!("OpenAI strict tool schema property must be an object");
        };

        let Some(kind) = object.remove("type") else {
            return Ok((schema, false));
        };
        let (kind, nullable) = match kind {
            Value::String(kind) if kind == "null" => {
                anyhow::bail!("OpenAI strict tool schema property cannot be null-only")
            }
            Value::String(kind) => (Value::String(kind), false),
            Value::Array(mut types) => {
                let nullable = types.iter().any(|kind| kind.as_str() == Some("null"));
                types.retain(|kind| kind.as_str() != Some("null"));
                let kind = match types.len() {
                    0 => anyhow::bail!("OpenAI strict tool schema property cannot be null-only"),
                    1 => types.remove(0),
                    _ => Value::Array(types),
                };
                (kind, nullable)
            }
            _ => anyhow::bail!("OpenAI strict tool schema property has an invalid type"),
        };
        object.insert("type".to_owned(), kind);

        Ok((schema, nullable))
    }

    fn make_nullable(schema: &mut Value) -> anyhow::Result<()> {
        let Value::Object(object) = schema else {
            anyhow::bail!("OpenAI strict tool schema property must be an object");
        };
        let Some(kind) = object.remove("type") else {
            anyhow::bail!("OpenAI strict tool schema cannot make an untyped property nullable");
        };

        let nullable_kind = match kind {
            Value::String(kind) if kind == "null" => {
                anyhow::bail!("OpenAI strict tool schema property cannot be null-only")
            }
            Value::String(kind) => {
                Value::Array(vec![Value::String(kind), Value::String("null".to_owned())])
            }
            Value::Array(mut kinds) => {
                if !kinds.iter().any(|kind| kind.as_str() == Some("null")) {
                    kinds.push(Value::String("null".to_owned()));
                }
                Value::Array(kinds)
            }
            _ => anyhow::bail!("OpenAI strict tool schema property has an invalid type"),
        };
        object.insert("type".to_owned(), nullable_kind);
        Ok(())
    }

    fn schemas_match_ignoring_descriptions(left: &Value, right: &Value) -> bool {
        fn strip_descriptions(value: &Value) -> Value {
            match value {
                Value::Object(object) => Value::Object(
                    object
                        .iter()
                        .filter(|(key, _)| !matches!(key.as_str(), "description" | "title"))
                        .map(|(key, value)| (key.clone(), strip_descriptions(value)))
                        .collect(),
                ),
                Value::Array(values) => {
                    Value::Array(values.iter().map(strip_descriptions).collect())
                }
                value => value.clone(),
            }
        }

        strip_descriptions(left) == strip_descriptions(right)
    }

    fn append_description(schema: &mut Map<String, Value>, heading: &str, lines: Vec<String>) {
        if lines.is_empty() {
            return;
        }

        let section = format!("{heading}:\n{}", lines.join("\n"));
        let description = schema
            .get("description")
            .and_then(Value::as_str)
            .filter(|description| !description.is_empty())
            .map(|description| format!("{description}\n\n{section}"))
            .unwrap_or(section);
        schema.insert("description".to_owned(), Value::String(description));
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

    fn compile_tool(spec: ToolDefinition) -> anyhow::Result<OpenAITool> {
        Ok(FunctionTool {
            name: spec.name,
            parameters: Some(Self::sanitize_schema(&spec.arguments)?),
            strict: Some(true),
            description: Some(spec.description),
            defer_loading: Some(false),
        }
        .into())
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

fn patch(v: &mut Value) {
    match v {
        Value::Object(obj) => {
            if obj.contains_key("text") && !obj.contains_key("annotations") {
                obj.insert("annotations".to_string(), json!([]));
            }

            for value in obj.values_mut() {
                patch(value)
            }
        }
        Value::Array(arr) => {
            for value in arr.iter_mut() {
                patch(value)
            }
        }
        _ => {}
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
        self.tools.extend(
            specs
                .into_iter()
                .map(Self::compile_tool)
                .collect::<anyhow::Result<Vec<_>>>()?,
        );
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
            .create_stream_byot::<_, Value>(request)
            .await?
            .map(|v| match v {
                Ok(mut value) => {
                    patch(&mut value);
                    let raw = serde_json::to_string(&value).unwrap();
                    serde_json::from_value::<ResponseStreamEvent>(value)
                        .map_err(|err| OpenAIError::JSONDeserialize(err, raw))
                }
                Err(e) => Err(e),
            })
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
    use crate::{
        provider::Provider,
        tool::{BashToolArgs, WriteFileToolArgs},
    };
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

        let sanitized = OpenAIProvider::sanitize_schema(&schema).unwrap();

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
        let sanitized = OpenAIProvider::sanitize_schema(&schema).unwrap();

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

    #[test]
    fn lowers_bash_tagged_union_for_openai_strict_tools() {
        let schema = serde_json::to_value(schemars::schema_for!(BashToolArgs)).unwrap();
        assert!(schema.get("oneOf").is_some());

        let sanitized = OpenAIProvider::sanitize_schema(&schema).unwrap();
        let description = sanitized["description"].as_str().unwrap();

        assert_eq!(sanitized["type"], "object");
        assert_eq!(sanitized["additionalProperties"], false);
        assert_eq!(
            sanitized["required"],
            json!(["action", "command", "input", "session_id"])
        );
        assert_eq!(
            sanitized["properties"]["action"],
            json!({
                "type": "string",
                "enum": [
                    "run_blocking",
                    "run_background",
                    "log_file_path",
                    "send",
                    "view",
                    "wait",
                    "terminate"
                ]
            })
        );
        assert_eq!(
            sanitized["properties"]["command"]["type"],
            json!(["string", "null"])
        );
        assert_eq!(
            sanitized["properties"]["input"]["type"],
            json!(["string", "null"])
        );
        assert_eq!(
            sanitized["properties"]["session_id"]["type"],
            json!(["string", "null"])
        );
        assert!(!sanitized.to_string().contains("oneOf"));
        assert!(!sanitized.to_string().contains("$defs"));

        for expected in [
            "`run_blocking`: Run a command and block until it completes. Required fields: `command`.",
            "`run_background`: Spawn a command in the background without waiting for completion. Required fields: `command`.",
            "`log_file_path`: Get the log file path containing a session's historical inputs and outputs. Required fields: `session_id`.",
            "`send`: Send input or a command to a running background terminal session. Required fields: `input`, `session_id`.",
            "`view`: Get the buffered output generated since the last view for a running session. Required fields: `session_id`.",
            "`wait`: Wait until the running command in a background terminal session exits. Required fields: `session_id`.",
            "`terminate`: Kill the running command in a background terminal session. Required fields: `session_id`.",
            "Set fields unused by the selected action to `null`.",
        ] {
            assert!(
                description.contains(expected),
                "missing {expected:?} in {description:?}"
            );
        }
    }

    #[test]
    fn lowers_documented_const_enum_and_aggregates_descriptions() {
        let schema = json!({
            "oneOf": [
                {"type": "string", "const": "alpha", "description": "Choose alpha."},
                {"type": "string", "const": "beta", "description": "Choose beta."}
            ]
        });

        let sanitized = OpenAIProvider::sanitize_schema(&schema).unwrap();

        assert_eq!(sanitized["type"], "string");
        assert_eq!(sanitized["enum"], json!(["alpha", "beta"]));
        assert_eq!(
            sanitized["description"],
            "Allowed values:\n- `alpha`: Choose alpha.\n- `beta`: Choose beta."
        );
    }

    #[test]
    fn lowers_nested_tagged_union() {
        let schema = json!({
            "type": "object",
            "properties": {
                "operation": {
                    "oneOf": [
                        {
                            "type": "object",
                            "description": "Read a value.",
                            "properties": {
                                "kind": {"type": "string", "const": "read"},
                                "path": {"type": "string"}
                            },
                            "required": ["kind", "path"]
                        },
                        {
                            "type": "object",
                            "description": "List values.",
                            "properties": {
                                "kind": {"type": "string", "const": "list"}
                            },
                            "required": ["kind"]
                        }
                    ]
                }
            },
            "required": ["operation"]
        });

        let sanitized = OpenAIProvider::sanitize_schema(&schema).unwrap();
        let operation = &sanitized["properties"]["operation"];

        assert_eq!(operation["type"], "object");
        assert_eq!(
            operation["properties"]["kind"]["enum"],
            json!(["read", "list"])
        );
        assert_eq!(
            operation["properties"]["path"]["type"],
            json!(["string", "null"])
        );
        assert!(!sanitized.to_string().contains("oneOf"));
    }

    #[test]
    fn rejects_unsupported_and_incompatible_one_of_schemas() {
        let arbitrary = json!({
            "oneOf": [
                {"type": "string"},
                {"type": "integer"}
            ]
        });
        let incompatible = json!({
            "oneOf": [
                {
                    "type": "object",
                    "properties": {
                        "kind": {"type": "string", "const": "text"},
                        "value": {"type": "string"}
                    },
                    "required": ["kind", "value"]
                },
                {
                    "type": "object",
                    "properties": {
                        "kind": {"type": "string", "const": "number"},
                        "value": {"type": "integer"}
                    },
                    "required": ["kind", "value"]
                }
            ]
        });

        assert!(
            OpenAIProvider::sanitize_schema(&arbitrary)
                .unwrap_err()
                .to_string()
                .contains("does not support oneOf")
        );
        assert!(
            OpenAIProvider::sanitize_schema(&incompatible)
                .unwrap_err()
                .to_string()
                .contains("incompatible schemas")
        );
    }
}
