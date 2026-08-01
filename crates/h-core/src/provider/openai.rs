use std::collections::{BTreeMap, BTreeSet};

use async_openai::{
    Client,
    config::OpenAIConfig,
    error::OpenAIError,
    types::responses::{
        EasyInputContent, EasyInputMessage, FunctionCallOutput, FunctionCallOutputItemParam,
        FunctionTool, FunctionToolCall, FunctionToolCallOutputResource, ImageDetail, InputContent,
        InputImageContent, InputItem, InputTextContent, Item, MessageType, OutputItem,
        OutputMessageContent, OutputStatus, Reasoning, ReasoningEffort as OpenAIReasoningEffort,
        ResponseStreamEvent, Role, Tool as OpenAITool, WebSearchTool, WebSearchToolCall,
        WebSearchToolCallAction, WebSearchToolCallStatus,
    },
};
use futures::StreamExt;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tiktoken_rs::{bpe_for_model, o200k_base_singleton};

use crate::{
    config::ReasoningEffort,
    context::{Message, Search, SearchAction, SearchSource, SearchStatus},
    event::{CompletedReason, ProviderSignal},
    input::{InputPart, UserInput},
    provider::{Compaction, Identity, Protocol, Provider, ProviderEventStream},
    tool::{ToolCall, ToolCallResult, ToolDefinition},
};

const COMPACT_PROMPT: &str = "Create a concise continuation state from the conversation. \
Preserve the current user request verbatim, decisions, constraints, exact file paths, code changes, \
relevant tool results, failures, and pending work. Clearly distinguish completed work from the next \
action the assistant must take. Never describe context compression itself as the user's task. Never \
invent tools, results, files, or decisions that are absent from the input. Do not continue the task \
or call tools. Output only the continuation state without wrapper tags.";

const SUMMARY_OPEN: &str = "<context_summary>";
const SUMMARY_CLOSE: &str = "</context_summary>";
const SUMMARY_CONTINUE: &str = "Continue the pending task using the context above.";

/// Used only when provider-native compacted items no longer carry dimensions.
const UNKNOWN_IMAGE_TOKENS: usize = 1024;

fn redact_images(value: &mut Value) -> usize {
    match value {
        Value::Array(values) => values.iter_mut().map(redact_images).sum(),
        Value::Object(object) => {
            let is_image = object.get("type").and_then(Value::as_str) == Some("input_image");

            if is_image && let Some(url) = object.get_mut("image_url") {
                *url = Value::String("[image data]".to_owned());
            }

            usize::from(is_image) + object.values_mut().map(redact_images).sum::<usize>()
        }
        _ => 0,
    }
}

fn reasoning_effort_name(effort: &OpenAIReasoningEffort) -> &'static str {
    match effort {
        OpenAIReasoningEffort::None => "none",
        OpenAIReasoningEffort::Minimal => "minimal",
        OpenAIReasoningEffort::Low => "low",
        OpenAIReasoningEffort::Medium => "medium",
        OpenAIReasoningEffort::High => "high",
        OpenAIReasoningEffort::Xhigh => "xhigh",
    }
}

impl From<ReasoningEffort> for OpenAIReasoningEffort {
    fn from(value: ReasoningEffort) -> Self {
        match value {
            ReasoningEffort::None => Self::None,
            ReasoningEffort::Minimal => Self::Minimal,
            ReasoningEffort::Low => Self::Low,
            ReasoningEffort::Medium => Self::Medium,
            ReasoningEffort::High => Self::High,
            ReasoningEffort::Xhigh | ReasoningEffort::Max => Self::Xhigh,
        }
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

fn is_unsupported_compact_error(error: &OpenAIError) -> bool {
    let OpenAIError::ApiError(response) = error else {
        return false;
    };

    matches!(
        response.status_code,
        reqwest::StatusCode::NOT_FOUND
            | reqwest::StatusCode::METHOD_NOT_ALLOWED
            | reqwest::StatusCode::NOT_IMPLEMENTED
    ) || response.api_error.code.as_deref() == Some("model_not_found")
        || response.api_error.message.contains("model_not_found")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CompactMode {
    Native,
    Model,
}

pub struct OpenAIProviderConfig {
    base_url: String,
    bearer_token: String,
    model: String,
    reasoning_effort: OpenAIReasoningEffort,
}

impl OpenAIProviderConfig {
    pub fn new(
        base_url: impl Into<String>,
        bearer_token: impl Into<String>,
        model: impl Into<String>,
        reasoning_effort: ReasoningEffort,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            bearer_token: bearer_token.into(),
            model: model.into(),
            reasoning_effort: reasoning_effort.into(),
        }
    }
}

pub struct OpenAIProvider {
    config: OpenAIProviderConfig,
    client: Client<OpenAIConfig>,
    tools: Vec<OpenAITool>,
    compact_mode: Mutex<CompactMode>,
}

#[derive(Serialize)]
struct ResponseRequest {
    model: String,
    input: Vec<Value>,
    tools: Vec<OpenAITool>,
    stream: bool,
    reasoning: Reasoning,
    include: Vec<&'static str>,
}

#[derive(Serialize)]
struct CompactRequest {
    model: String,
    input: Vec<Value>,
}

#[derive(Serialize)]
struct SummaryRequest {
    model: String,
    input: Vec<Value>,
    stream: bool,
}

#[derive(Deserialize)]
struct CompactResponse {
    output: Vec<Value>,
}

impl OpenAIProvider {
    pub fn from_config(config: OpenAIProviderConfig) -> Self {
        let client_config = OpenAIConfig::new()
            .with_api_base(&config.base_url)
            .with_api_key(&config.bearer_token);

        let client = Client::with_config(client_config);

        let tools = vec![async_openai::types::responses::Tool::WebSearch(
            WebSearchTool::default(),
        )];

        Self {
            config,
            client,
            tools,
            compact_mode: Mutex::new(CompactMode::Native),
        }
    }

    fn signal(event: ResponseStreamEvent) -> anyhow::Result<ProviderSignal> {
        match event {
            ResponseStreamEvent::ResponseOutputItemDone(done) => match done.item {
                OutputItem::FunctionCall(call) => {
                    tracing::info!(
                        event = "provider.tool_call.requested",
                        provider = "openai",
                        tool_name = call.name
                    );
                    let arguments = serde_json::from_str(&call.arguments)?;

                    Ok(ProviderSignal::ToolCallStarted(ToolCall::new(
                        call.call_id,
                        call.name,
                        arguments,
                    )))
                }
                OutputItem::FunctionCallOutput(call) => {
                    let output = match call.output {
                        FunctionCallOutput::Text(text) => {
                            serde_json::from_str(&text).unwrap_or(Value::String(text))
                        }
                        FunctionCallOutput::Content(content) => serde_json::to_value(content)?,
                    };

                    Ok(ProviderSignal::ToolCallCompleted(ToolCallResult::success(
                        call.call_id,
                        output,
                    )))
                }
                item @ OutputItem::Reasoning(_) => {
                    Ok(ProviderSignal::Reasoning(serde_json::to_vec(&item)?))
                }
                OutputItem::WebSearchCall(call) => Ok(ProviderSignal::Search(search(call)?)),
                _ => Ok(ProviderSignal::Unsupported),
            },
            ResponseStreamEvent::ResponseOutputTextDelta(delta) => {
                Ok(ProviderSignal::TextDelta(delta.delta))
            }
            ResponseStreamEvent::ResponseCompleted(completed) => {
                let need_call = completed
                    .response
                    .output
                    .iter()
                    .any(|item| matches!(item, OutputItem::FunctionCall(_)));
                tracing::info!(
                    event = "provider.response.completed",
                    provider = "openai",
                    completion_reason = if need_call {
                        "needs_tool_call"
                    } else {
                        "final"
                    },
                );

                Ok(ProviderSignal::Completed {
                    reason: if need_call {
                        CompletedReason::NeedCall
                    } else {
                        CompletedReason::Final
                    },
                })
            }
            ResponseStreamEvent::ResponseFailed(failed) => {
                let message = failed
                    .response
                    .error
                    .as_ref()
                    .map(|error| error.message.as_str())
                    .unwrap_or("provider response failed");

                anyhow::bail!(message.to_owned())
            }
            ResponseStreamEvent::ResponseIncomplete(incomplete) => {
                let reason = incomplete
                    .response
                    .incomplete_details
                    .as_ref()
                    .map(|details| details.reason.as_str())
                    .unwrap_or("unknown reason");

                anyhow::bail!("provider response incomplete: {reason}")
            }
            ResponseStreamEvent::ResponseError(error) => {
                anyhow::bail!("provider stream error: {}", error.message)
            }
            _ => Ok(ProviderSignal::Unsupported),
        }
    }

    fn input(messages: &[Message]) -> anyhow::Result<Vec<Value>> {
        let mut input = Vec::with_capacity(messages.len());

        for message in messages {
            match message {
                Message::Compaction(compaction) => {
                    let items = serde_json::from_slice::<Vec<Value>>(compaction.state())?;
                    input.extend(items);
                }
                Message::Reasoning(item) => input.push(serde_json::from_slice(item)?),
                Message::Search(search) => input.push(serde_json::from_slice(search.state())?),
                message => input.push(serde_json::to_value(Self::input_item(message.clone()))?),
            }
        }

        Ok(input)
    }

    fn input_item(message: Message) -> InputItem {
        match message {
            Message::User(input) => Self::user_input_item(input),
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
            Message::Reasoning(_) => {
                unreachable!("reasoning items are expanded before item conversion")
            }
            Message::Search(_) => {
                unreachable!("search items are expanded before item conversion")
            }
            Message::ToolCall {
                call_id,
                arguments,
                name,
            } => InputItem::Item(Item::FunctionCall(FunctionToolCall {
                call_id,
                arguments,
                namespace: None,
                name,
                id: None,
                status: Some(OutputStatus::Completed),
            })),
            Message::ToolCallResult {
                call_id, output, ..
            } => InputItem::Item(Item::FunctionCallOutput(FunctionCallOutputItemParam {
                call_id,
                output: FunctionCallOutput::Text(output),
                id: None,
                status: Some(OutputStatus::Completed),
            })),
            Message::Compaction(_) => {
                unreachable!("compaction messages are expanded before item conversion")
            }
        }
    }

    fn user_input_item(input: UserInput) -> InputItem {
        if !input.has_images() {
            return EasyInputMessage::from(input.text()).into();
        }

        let mut content = Vec::with_capacity(input.parts().len() * 2);
        let mut image_index = 0_usize;

        for part in input.parts() {
            match part {
                InputPart::Text(text) if !text.is_empty() => {
                    content.push(InputContent::InputText(InputTextContent {
                        text: text.clone(),
                    }));
                }
                InputPart::Text(_) => {}
                InputPart::Image(image) => {
                    image_index += 1;
                    content.push(InputContent::InputText(InputTextContent {
                        text: format!("[Image {image_index}]"),
                    }));
                    content.push(InputContent::InputImage(InputImageContent {
                        detail: ImageDetail::Auto,
                        file_id: None,
                        image_url: Some(image.data_url()),
                    }));
                }
            }
        }

        EasyInputMessage {
            r#type: MessageType::Message,
            role: Role::User,
            content: EasyInputContent::ContentList(content),
            phase: None,
        }
        .into()
    }

    fn estimate_values(&self, values: &[Value]) -> anyhow::Result<usize> {
        if values.is_empty() {
            return Ok(0);
        }

        let mut values = values.to_vec();
        let image_count = values.iter_mut().map(redact_images).sum::<usize>();
        let input = serde_json::to_string(&values)?;
        let tokenizer =
            bpe_for_model(&self.config.model).unwrap_or_else(|_| o200k_base_singleton());

        Ok(tokenizer
            .count_ordinary(&input)
            .saturating_add(image_count.saturating_mul(UNKNOWN_IMAGE_TOKENS)))
    }

    fn estimate_messages(&self, messages: &[Message]) -> anyhow::Result<usize> {
        let (image_count, image_tokens) = messages
            .iter()
            .filter_map(|message| match message {
                Message::User(input) => Some(input.images()),
                _ => None,
            })
            .flatten()
            .fold((0_usize, 0_usize), |(count, tokens), image| {
                (
                    count.saturating_add(1),
                    tokens.saturating_add(image.estimated_tokens()),
                )
            });
        let estimate = self.estimate_values(&Self::input(messages)?)?;

        Ok(estimate
            .saturating_sub(image_count.saturating_mul(UNKNOWN_IMAGE_TOKENS))
            .saturating_add(image_tokens))
    }

    fn build_request(&self, input: Vec<Value>) -> anyhow::Result<ResponseRequest> {
        Ok(ResponseRequest {
            model: self.config.model.clone(),
            input,
            tools: self.tools.clone(),
            stream: true,
            reasoning: Reasoning::from(self.config.reasoning_effort.clone()),
            include: vec![
                "reasoning.encrypted_content",
                "web_search_call.action.sources",
            ],
        })
    }

    fn build_summary_request(&self, input: Vec<Value>) -> anyhow::Result<SummaryRequest> {
        let instruction =
            serde_json::to_value(Self::input_item(Message::System(COMPACT_PROMPT.to_owned())))?;
        let mut messages = Vec::with_capacity(input.len() + 1);

        messages.push(instruction);
        messages.extend(input);

        Ok(SummaryRequest {
            model: self.config.model.clone(),
            input: messages,
            stream: false,
        })
    }

    fn summary_state(output: Vec<Value>) -> anyhow::Result<Vec<u8>> {
        let text = output
            .into_iter()
            .filter_map(|item| serde_json::from_value::<OutputItem>(item).ok())
            .filter_map(|item| match Message::try_from(item) {
                Ok(Message::Assistant(text)) => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        if text.trim().is_empty() {
            anyhow::bail!("the compaction model returned no text");
        }

        let summary = format!(
            "{SUMMARY_OPEN}\n{}\n{SUMMARY_CLOSE}\n\n{SUMMARY_CONTINUE}",
            text.trim()
        );
        let item = serde_json::to_value(Self::input_item(Message::User(summary.into())))?;
        Ok(serde_json::to_vec(&vec![item])?)
    }

    async fn compact_with_model(&self, input: Vec<Value>) -> anyhow::Result<Compaction> {
        let request = self.build_summary_request(input)?;
        let input_tokens = self.estimate_values(&request.input)?;
        let response: CompactResponse = self.client.responses().create_byot(request).await?;
        let output_tokens = self.estimate_values(&response.output)?;
        let state = Self::summary_state(response.output)?;
        let total_tokens = input_tokens.saturating_add(output_tokens);

        tracing::info!(
            event = "provider.context.compacted",
            provider = "openai",
            method = "model",
            state_bytes = state.len(),
            estimated_input_tokens = input_tokens,
            estimated_output_tokens = output_tokens,
            estimated_total_tokens = total_tokens,
        );

        Ok(Compaction::new(state, Some(total_tokens)))
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

        super::schema::sanitize(&schema)
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
            && let Some(name) = reference
                .strip_prefix("#/$defs/")
                .or_else(|| reference.strip_prefix("#/definitions/"))
            && let Some(definition) = definitions.get(name)
        {
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

        if Self::is_open_schema(object) {
            object.insert("type".to_owned(), Value::String("object".to_owned()));
            return Ok(());
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

    fn is_open_schema(schema: &Map<String, Value>) -> bool {
        [
            "type",
            "$ref",
            "const",
            "enum",
            "oneOf",
            "anyOf",
            "allOf",
            "properties",
            "items",
        ]
        .iter()
        .all(|keyword| !schema.contains_key(*keyword))
    }

    fn supports_strict(schema: &Value) -> bool {
        let Value::Object(schema) = schema else {
            return true;
        };

        if Self::has_type(schema, "object")
            && schema.get("additionalProperties") != Some(&Value::Bool(false))
        {
            return false;
        }

        if let Some(Value::Object(properties)) = schema.get("properties")
            && properties
                .values()
                .any(|property| !Self::supports_strict(property))
        {
            return false;
        }

        if let Some(items) = schema.get("items")
            && !Self::supports_strict(items)
        {
            return false;
        }

        for keyword in ["anyOf", "allOf"] {
            if let Some(Value::Array(branches)) = schema.get(keyword)
                && branches.iter().any(|branch| !Self::supports_strict(branch))
            {
                return false;
            }
        }

        true
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
        let parameters = Self::sanitize_schema(&spec.arguments)?;
        let strict = Self::supports_strict(&parameters);

        Ok(FunctionTool {
            name: spec.name,
            parameters: Some(parameters),
            strict: Some(strict),
            description: Some(spec.description),
            defer_loading: Some(false),
        }
        .into())
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
                    summary: None,
                })
            }
            OutputItem::Reasoning(reasoning) => {
                let item = OutputItem::Reasoning(reasoning);
                Ok(Message::Reasoning(serde_json::to_vec(&item)?))
            }
            OutputItem::WebSearchCall(call) => Ok(Message::Search(search(call)?)),
            _ => anyhow::bail!("Unsupported Message"),
        }
    }
}

fn search(call: WebSearchToolCall) -> anyhow::Result<Search> {
    let status = match call.status {
        WebSearchToolCallStatus::InProgress | WebSearchToolCallStatus::Searching => {
            SearchStatus::Running
        }
        WebSearchToolCallStatus::Completed => SearchStatus::Succeeded,
        WebSearchToolCallStatus::Failed => SearchStatus::Failed,
    };
    let action = call.action.as_ref().map(|action| match action {
        WebSearchToolCallAction::Search(search) => SearchAction::Query {
            query: search.query.clone(),
            sources: search
                .sources
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|source| SearchSource::new(source.url.clone(), None))
                .collect(),
        },
        WebSearchToolCallAction::OpenPage(open) => SearchAction::Open {
            url: open.url.clone(),
        },
        WebSearchToolCallAction::Find(find) | WebSearchToolCallAction::FindInPage(find) => {
            SearchAction::Find {
                url: find.url.clone(),
                pattern: find.pattern.clone(),
            }
        }
    });
    let (id, state) = (
        call.id.clone(),
        serde_json::to_vec(&OutputItem::WebSearchCall(call))?,
    );

    Ok(Search::new(id, status, action, state))
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
    fn model(&self) -> &str {
        &self.config.model
    }

    fn identity(&self) -> Option<Identity> {
        Some(Identity {
            protocol: Protocol::OpenAI,
            base_url: self.config.base_url.clone(),
        })
    }

    fn thinking_effort(&self) -> Option<&str> {
        Some(reasoning_effort_name(&self.config.reasoning_effort))
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

    fn estimate_request_tokens(&self, input: &[Message]) -> anyhow::Result<Option<usize>> {
        let (input_tokens, tools) = (
            self.estimate_messages(input)?,
            serde_json::to_string(&self.tools)?,
        );
        let tokenizer =
            bpe_for_model(&self.config.model).unwrap_or_else(|_| o200k_base_singleton());

        Ok(Some(
            input_tokens.saturating_add(tokenizer.count_ordinary(&tools)),
        ))
    }

    fn estimate_output_tokens(&self, output: &[Message]) -> anyhow::Result<Option<usize>> {
        self.estimate_messages(output).map(Some)
    }

    async fn compact(&self, messages: &[Message]) -> anyhow::Result<Option<Compaction>> {
        let input = Self::input(messages)?;
        if input.is_empty() {
            return Ok(None);
        }

        if *self.compact_mode.lock() == CompactMode::Model {
            return self.compact_with_model(input).await.map(Some);
        }

        let request = CompactRequest {
            model: self.config.model.clone(),
            input: input.clone(),
        };
        let input_tokens = self.estimate_values(&input)?;
        let response: CompactResponse = match self.client.responses().compact_byot(request).await {
            Ok(response) => response,
            Err(error) if is_unsupported_compact_error(&error) => {
                *self.compact_mode.lock() = CompactMode::Model;

                tracing::warn!(
                    event = "provider.context.compact_fallback",
                    provider = "openai",
                    reason = "native_unsupported",
                    error = error.to_string(),
                );

                return self.compact_with_model(input).await.map(Some);
            }
            Err(error) => return Err(error.into()),
        };
        let item_count = response.output.len();
        let output_tokens = self.estimate_values(&response.output)?;
        let state = serde_json::to_vec(&response.output)?;
        let total_tokens = input_tokens.saturating_add(output_tokens);

        tracing::info!(
            event = "provider.context.compacted",
            provider = "openai",
            method = "native",
            item_count,
            state_bytes = state.len(),
            estimated_input_tokens = input_tokens,
            estimated_output_tokens = output_tokens,
            estimated_total_tokens = total_tokens,
        );

        Ok(Some(Compaction::new(state, Some(total_tokens))))
    }

    async fn stream(&self, messages: &[Message]) -> anyhow::Result<ProviderEventStream> {
        let message_count = messages.len();
        let tool_count = self.tools.len();
        let input = Self::input(messages)?;

        let request = self.build_request(input)?;

        // Keep this as a foreground Responses request. OpenAI's explicit
        // `/cancel` endpoint is background-only; synchronous cancellation is
        // performed by dropping this stream, which closes the SSE connection.
        let stream = self
            .client
            .responses()
            .create_stream_byot::<_, Value>(request)
            .await?
            .map(|event| match event {
                Ok(mut value) => {
                    patch(&mut value);
                    let raw = serde_json::to_string(&value).unwrap();
                    serde_json::from_value::<ResponseStreamEvent>(value)
                        .map_err(|error| OpenAIError::JSONDeserialize(error, raw))
                }
                Err(error) => Err(error),
            })
            .filter_map(|result| async move {
                match result {
                    Ok(event) => Some(Self::signal(event)),
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
        input::Image,
        provider::Provider,
        tool::{BashToolArgs, WriteFileToolArgs},
    };
    use async_openai::error::{ApiError, ApiErrorResponse};
    use serde_json::json;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };

    async fn read_request(stream: &mut TcpStream) -> (String, Value) {
        let mut request = Vec::new();

        let (header_end, content_len) = loop {
            let mut chunk = [0; 4_096];
            let len = stream.read(&mut chunk).await.unwrap();
            assert!(len > 0, "the client closed before sending HTTP headers");
            request.extend_from_slice(&chunk[..len]);

            let Some(header_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
                continue;
            };
            let headers = str::from_utf8(&request[..header_end]).unwrap();
            let content_len = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(str::trim)
                        .and_then(|len| len.parse::<usize>().ok())
                })
                .unwrap_or_default();

            break (header_end, content_len);
        };

        let body_start = header_end + 4;
        while request.len() < body_start + content_len {
            let mut chunk = [0; 4_096];
            let len = stream.read(&mut chunk).await.unwrap();
            assert!(len > 0, "the client closed before sending the HTTP body");
            request.extend_from_slice(&chunk[..len]);
        }

        let headers = str::from_utf8(&request[..header_end]).unwrap();
        let path = headers
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .to_owned();
        let body = serde_json::from_slice(&request[body_start..body_start + content_len]).unwrap();

        (path, body)
    }

    async fn respond(stream: &mut TcpStream, status: &str, body: Value) {
        let body = body.to_string();
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",
            body.len(),
        );

        stream.write_all(response.as_bytes()).await.unwrap();
    }

    fn provider(reasoning_effort: ReasoningEffort) -> OpenAIProvider {
        OpenAIProvider::from_config(OpenAIProviderConfig::new(
            "https://example.com",
            "secret",
            "gpt-5.6-sol",
            reasoning_effort,
        ))
    }

    #[test]
    fn maps_configured_reasoning_efforts() {
        for (configured, effective) in [
            (ReasoningEffort::None, "none"),
            (ReasoningEffort::Minimal, "minimal"),
            (ReasoningEffort::Low, "low"),
            (ReasoningEffort::Medium, "medium"),
            (ReasoningEffort::High, "high"),
            (ReasoningEffort::Xhigh, "xhigh"),
            (ReasoningEffort::Max, "xhigh"),
        ] {
            let provider = provider(configured);

            assert_eq!(provider.thinking_effort(), Some(effective));
        }
    }

    #[test]
    fn request_and_accessors_use_configured_reasoning_effort() {
        let provider = provider(ReasoningEffort::High);
        let request = provider.build_request(Vec::new()).unwrap();
        let value = serde_json::to_value(request).unwrap();

        assert_eq!(provider.model(), "gpt-5.6-sol");
        assert_eq!(provider.thinking_effort(), Some("high"));
        assert_eq!(value["model"], "gpt-5.6-sol");
        assert_eq!(value["reasoning"]["effort"], "high");
        assert_eq!(
            value["include"],
            json!([
                "reasoning.encrypted_content",
                "web_search_call.action.sources"
            ])
        );
        assert!(!value.to_string().contains("secret"));
    }

    #[test]
    fn tokenizer_estimates_requests_and_outputs_locally() {
        let provider = provider(ReasoningEffort::High);
        let messages = [
            Message::System("You are a coding agent.".to_owned()),
            Message::User("Inspect the repository.".into()),
        ];
        let (empty_request, request, output) = (
            provider.estimate_request_tokens(&[]).unwrap().unwrap(),
            provider
                .estimate_request_tokens(&messages)
                .unwrap()
                .unwrap(),
            provider.estimate_output_tokens(&messages).unwrap().unwrap(),
        );

        assert!(empty_request > 0, "registered tools occupy request context");
        assert!(request > empty_request, "message text must add tokens");
        assert!(
            request > output,
            "output estimates exclude request-only tools"
        );
    }

    #[test]
    fn image_inputs_keep_order_and_stable_labels() {
        let input = UserInput::new(vec![
            InputPart::Text("compare ".to_owned()),
            InputPart::Image(Image::new("image/png", [1, 2, 3], 32, 32).unwrap()),
            InputPart::Text(" with ".to_owned()),
            InputPart::Image(Image::new("image/jpeg", [4, 5, 6], 64, 32).unwrap()),
        ]);

        let values = OpenAIProvider::input(&[Message::User(input)]).unwrap();
        let content = values[0]["content"].as_array().unwrap();

        assert_eq!(content.len(), 6);
        assert_eq!(
            content[0],
            json!({ "type": "input_text", "text": "compare " })
        );
        assert_eq!(
            content[1],
            json!({ "type": "input_text", "text": "[Image 1]" })
        );
        assert_eq!(content[2]["type"], "input_image");
        assert_eq!(content[2]["detail"], "auto");
        assert!(
            content[2]["image_url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        assert_eq!(
            content[3],
            json!({ "type": "input_text", "text": " with " })
        );
        assert_eq!(
            content[4],
            json!({ "type": "input_text", "text": "[Image 2]" })
        );
        assert!(
            content[5]["image_url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/jpeg;base64,")
        );
    }

    #[test]
    fn base64_payload_is_not_tokenized_as_text() {
        let provider = provider(ReasoningEffort::High);
        let small = UserInput::from_text_and_images(
            "inspect".to_owned(),
            vec![Image::new("image/png", vec![0; 16], 64, 64).unwrap()],
        );
        let large = UserInput::from_text_and_images(
            "inspect".to_owned(),
            vec![Image::new("image/png", vec![0; 100_000], 64, 64).unwrap()],
        );
        let (small, large) = (
            provider
                .estimate_output_tokens(&[Message::User(small)])
                .unwrap(),
            provider
                .estimate_output_tokens(&[Message::User(large)])
                .unwrap(),
        );

        assert_eq!(small, large);
    }

    #[test]
    fn compacted_windows_are_expanded_without_pruning_items() {
        let compacted = json!([
            {
                "id": "msg-1",
                "type": "message",
                "status": "completed",
                "content": [{"type": "input_text", "text": "old prompt"}],
                "role": "user"
            },
            {
                "id": "cmp-1",
                "type": "compaction",
                "encrypted_content": "opaque"
            }
        ]);
        let messages = [
            Message::System("instructions".to_owned()),
            Message::Compaction(Compaction::new(
                serde_json::to_vec(&compacted).unwrap(),
                None,
            )),
            Message::User("new prompt".into()),
        ];

        let input = OpenAIProvider::input(&messages).unwrap();

        assert_eq!(input.len(), 4);
        assert_eq!(input[1], compacted[0]);
        assert_eq!(input[2], compacted[1]);
        assert_eq!(input[3]["role"], "user");
        assert_eq!(input[3]["content"], "new prompt");
    }

    #[test]
    fn reasoning_items_are_replayed_before_their_tool_calls_and_results() {
        let reasoning = json!({
            "type": "reasoning",
            "id": "rs-1",
            "summary": [],
            "encrypted_content": "opaque",
            "status": "completed"
        });
        let messages = [
            Message::Reasoning(serde_json::to_vec(&reasoning).unwrap()),
            Message::ToolCall {
                call_id: "call-1".to_owned(),
                name: "read_file".to_owned(),
                arguments: "{\"path\":\"src/main.rs\"}".to_owned(),
            },
            Message::ToolCall {
                call_id: "call-2".to_owned(),
                name: "read_file".to_owned(),
                arguments: "{\"path\":\"src/lib.rs\"}".to_owned(),
            },
            Message::ToolCallResult {
                call_id: "call-1".to_owned(),
                output: "{\"content\":\"fn main() {}\"}".to_owned(),
                summary: None,
            },
            Message::ToolCallResult {
                call_id: "call-2".to_owned(),
                output: "{\"content\":\"pub mod agent;\"}".to_owned(),
                summary: None,
            },
        ];

        let input = OpenAIProvider::input(&messages).unwrap();

        assert_eq!(input.len(), 5);
        assert_eq!(input[0], reasoning);
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[1]["call_id"], "call-1");
        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[2]["call_id"], "call-2");
        assert_eq!(input[3]["type"], "function_call_output");
        assert_eq!(input[3]["call_id"], "call-1");
        assert_eq!(input[4]["type"], "function_call_output");
        assert_eq!(input[4]["call_id"], "call-2");
    }

    #[test]
    fn search_items_are_replayed_with_their_provider_state() {
        let item = json!({
            "type": "web_search_call",
            "id": "ws-1",
            "status": "completed",
            "action": {
                "type": "search",
                "query": "Rust async runtimes",
                "sources": [
                    { "type": "url", "url": "https://tokio.rs" }
                ]
            }
        });
        let output = serde_json::from_value::<OutputItem>(item.clone()).unwrap();
        let message = Message::try_from(output).unwrap();

        let input = OpenAIProvider::input(&[message]).unwrap();

        assert_eq!(input, [item]);
    }

    #[test]
    fn completed_reasoning_items_emit_an_opaque_reasoning_signal() {
        let reasoning = json!({
            "type": "reasoning",
            "id": "rs-1",
            "summary": [],
            "encrypted_content": "opaque",
            "status": "completed"
        });
        let event = serde_json::from_value::<ResponseStreamEvent>(json!({
            "type": "response.output_item.done",
            "sequence_number": 3,
            "output_index": 0,
            "item": reasoning.clone()
        }))
        .unwrap();
        let signal = OpenAIProvider::signal(event).unwrap();

        assert!(matches!(
            signal,
            ProviderSignal::Reasoning(item)
                if serde_json::from_slice::<Value>(&item).unwrap() == reasoning
        ));
    }

    #[test]
    fn completed_search_items_emit_a_search_signal_with_sources() {
        let item = json!({
            "type": "web_search_call",
            "id": "ws-1",
            "status": "completed",
            "action": {
                "type": "search",
                "query": "Rust async runtimes",
                "sources": [
                    { "type": "url", "url": "https://tokio.rs" },
                    { "type": "url", "url": "https://async.rs" }
                ]
            }
        });
        let event = serde_json::from_value::<ResponseStreamEvent>(json!({
            "type": "response.output_item.done",
            "sequence_number": 3,
            "output_index": 0,
            "item": item.clone()
        }))
        .unwrap();
        let signal = OpenAIProvider::signal(event).unwrap();

        let ProviderSignal::Search(search) = signal else {
            panic!("expected a search signal");
        };
        assert_eq!(search.id(), "ws-1");
        assert_eq!(search.status(), SearchStatus::Succeeded);
        assert_eq!(
            serde_json::from_slice::<Value>(search.state()).unwrap(),
            item
        );
        assert!(matches!(
            search.action(),
            Some(SearchAction::Query { query, sources })
                if query == "Rust async runtimes"
                    && sources.iter().map(SearchSource::url).collect::<Vec<_>>()
                        == ["https://tokio.rs", "https://async.rs"]
        ));
    }

    #[test]
    fn compact_requests_use_input_messages_instead_of_instructions() {
        let request = CompactRequest {
            model: "gpt-5.6-sol".to_owned(),
            input: vec![json!({"role": "user", "content": "old prompt"})],
        };
        let value = serde_json::to_value(request).unwrap();

        assert_eq!(value["model"], "gpt-5.6-sol");
        assert_eq!(value["input"][0]["role"], "user");
        assert!(value.get("instructions").is_none());
    }

    #[test]
    fn model_not_found_disables_native_compaction() {
        let error = OpenAIError::ApiError(ApiErrorResponse {
            status_code: reqwest::StatusCode::SERVICE_UNAVAILABLE,
            api_error: ApiError {
                message: "the compact route has no channel".to_owned(),
                r#type: Some("new_api_error".to_owned()),
                param: None,
                code: Some("model_not_found".to_owned()),
            },
        });

        assert!(is_unsupported_compact_error(&error));
    }

    #[test]
    fn summary_requests_use_a_system_message_without_tools() {
        let provider = provider(ReasoningEffort::High);
        let input = OpenAIProvider::input(&[Message::User("old prompt".into())]).unwrap();
        let request = provider.build_summary_request(input).unwrap();
        let value = serde_json::to_value(request).unwrap();

        assert_eq!(value["model"], "gpt-5.6-sol");
        assert_eq!(value["stream"], false);
        assert_eq!(value["input"][0]["role"], "system");
        assert_eq!(value["input"][0]["content"], COMPACT_PROMPT);
        assert_eq!(value["input"][1]["role"], "user");
        assert!(value.get("tools").is_none());
        assert!(value.get("instructions").is_none());
    }

    #[tokio::test]
    async fn unsupported_native_compaction_falls_back_to_a_normal_response() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let mut requests = Vec::new();

            for index in 0..5 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = read_request(&mut stream).await;
                requests.push(request);

                if index < 4 {
                    respond(
                        &mut stream,
                        "503 Service Unavailable",
                        json!({
                            "error": {
                                "message": "the compact route has no channel",
                                "type": "new_api_error",
                                "param": null,
                                "code": "model_not_found"
                            }
                        }),
                    )
                    .await;
                } else {
                    respond(
                        &mut stream,
                        "200 OK",
                        json!({
                            "output": [{
                                "type": "message",
                                "id": "msg-1",
                                "status": "completed",
                                "role": "assistant",
                                "content": [{
                                    "type": "output_text",
                                    "text": "Current task: inspect the project.",
                                    "annotations": []
                                }]
                            }]
                        }),
                    )
                    .await;
                }
            }

            requests
        });

        let provider = OpenAIProvider::from_config(OpenAIProviderConfig::new(
            format!("http://{address}"),
            "secret",
            "gpt-5.6-sol",
            ReasoningEffort::High,
        ));

        let compaction = provider
            .compact(&[Message::User("inspect the project".into())])
            .await
            .unwrap()
            .unwrap();
        let requests = server.await.unwrap();
        let state = serde_json::from_slice::<Vec<Value>>(compaction.state()).unwrap();

        assert!(
            requests[..4]
                .iter()
                .all(|request| request.0 == "/responses/compact")
        );
        assert_eq!(requests[4].0, "/responses");
        assert_eq!(requests[4].1["input"][0]["role"], "system");
        assert_eq!(requests[4].1["input"][1]["role"], "user");
        assert_eq!(*provider.compact_mode.lock(), CompactMode::Model);
        assert_eq!(state.len(), 1);
        assert_eq!(state[0]["role"], "user");
        assert_eq!(
            state[0]["content"],
            "<context_summary>\nCurrent task: inspect the project.\n</context_summary>\n\nContinue the pending task using the context above."
        );
        assert!(compaction.total_tokens().is_some_and(|tokens| tokens > 0));
    }

    #[tokio::test]
    async fn native_compaction_reports_a_local_token_estimate() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_request(&mut stream).await;

            respond(
                &mut stream,
                "200 OK",
                json!({
                    "output": [{
                        "type": "compaction",
                        "id": "cmp-1",
                        "encrypted_content": "opaque"
                    }]
                }),
            )
            .await;

            request
        });

        let provider = OpenAIProvider::from_config(OpenAIProviderConfig::new(
            format!("http://{address}"),
            "secret",
            "gpt-5.6-sol",
            ReasoningEffort::High,
        ));

        let compaction = provider
            .compact(&[Message::User("inspect the project".into())])
            .await
            .unwrap()
            .unwrap();

        let request = server.await.unwrap();

        assert_eq!(request.0, "/responses/compact");
        assert!(compaction.total_tokens().is_some_and(|tokens| tokens > 0));
    }

    #[test]
    fn max_reasoning_effort_is_sent_as_xhigh() {
        let provider = provider(ReasoningEffort::Max);
        let request = provider.build_request(Vec::new()).unwrap();
        let value = serde_json::to_value(request).unwrap();

        assert_eq!(value["reasoning"]["effort"], "xhigh");
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
    fn uses_non_strict_mode_for_open_object_properties() {
        let tool = OpenAIProvider::compile_tool(ToolDefinition {
            name: "get_outgoing_calls".to_owned(),
            description: "Get outgoing calls.".to_owned(),
            arguments: json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "properties": {
                    "item": {
                        "description": "The call hierarchy item to get calls for."
                    }
                },
                "required": ["item"]
            }),
        })
        .unwrap();
        let tool = serde_json::to_value(tool).unwrap();
        let item = &tool["parameters"]["properties"]["item"];

        assert_eq!(item["type"], "object");
        assert!(item.get("additionalProperties").is_none());
        assert_eq!(tool["strict"], false);
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
            json!(["action", "brief", "command", "input", "session_id"])
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
            sanitized["properties"]["brief"]["type"],
            json!(["boolean", "null"])
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
