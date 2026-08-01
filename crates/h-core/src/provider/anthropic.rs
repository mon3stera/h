use std::collections::HashMap;

use anthropic_rust_sdk::{
    Anthropic, ClientOptions, ContentBlockParam, MessageContent, MessageCreateParams,
    MessageCreateResult, MessageParam, RawMessageStreamEvent, Role,
};
use futures::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tiktoken_rs::{bpe_for_model, o200k_base_singleton};

use crate::{
    config::ReasoningEffort,
    context::{Message, Search, SearchAction, SearchSource, SearchStatus},
    event::{CompletedReason, ProviderSignal},
    input::{InputPart, UserInput},
    provider::{Compaction, Identity, Protocol, Provider, ProviderEventStream},
    tool::{ToolCall, ToolDefinition},
};

const MAX_OUTPUT_TOKENS: u64 = 32_768;
const MAX_COMPACT_OUTPUT_TOKENS: u64 = 8_192;
const UNKNOWN_IMAGE_TOKENS: usize = 1024;
const COMPACT_PROMPT: &str = "Create a concise continuation state from the conversation. \
Preserve the current user request verbatim, decisions, constraints, exact file paths, code changes, \
relevant tool results, failures, and pending work. Clearly distinguish completed work from the next \
action the assistant must take. Never describe context compression itself as the user's task. Never \
invent tools, results, files, or decisions that are absent from the input. Do not continue the task \
or call tools. Output only the continuation state without wrapper tags.";
const SUMMARY_OPEN: &str = "<context_summary>";
const SUMMARY_CLOSE: &str = "</context_summary>";
const SUMMARY_CONTINUE: &str = "Continue the pending task using the context above.";

fn effort_name(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::None => "none",
        ReasoningEffort::Minimal => "minimal",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::Xhigh => "xhigh",
        ReasoningEffort::Max => "max",
    }
}

fn thinking(effort: ReasoningEffort) -> Value {
    let budget = match effort {
        ReasoningEffort::None => return json!({ "type": "disabled" }),
        ReasoningEffort::Minimal => 1_024,
        ReasoningEffort::Low => 2_048,
        ReasoningEffort::Medium => 4_096,
        ReasoningEffort::High => 8_192,
        ReasoningEffort::Xhigh | ReasoningEffort::Max => 16_384,
    };

    json!({
        "type": "enabled",
        "budget_tokens": budget,
    })
}

pub struct AnthropicProviderConfig {
    base_url: String,
    api_key: Option<String>,
    auth_token: Option<String>,
    model: String,
    reasoning_effort: ReasoningEffort,
}

impl AnthropicProviderConfig {
    pub fn new(
        base_url: impl Into<String>,
        api_key: Option<String>,
        auth_token: Option<String>,
        model: impl Into<String>,
        reasoning_effort: ReasoningEffort,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            api_key,
            auth_token,
            model: model.into(),
            reasoning_effort,
        }
    }
}

pub struct AnthropicProvider {
    config: AnthropicProviderConfig,
    client: Anthropic,
    tools: Vec<Value>,
}

#[derive(Deserialize, Serialize)]
struct StoredCompaction {
    protocol: String,
    messages: Vec<MessageParam>,
}

impl AnthropicProvider {
    pub fn from_config(config: AnthropicProviderConfig) -> anyhow::Result<Self> {
        let client = Anthropic::with_options(ClientOptions {
            api_key: config.api_key.clone(),
            auth_token: config.auth_token.clone(),
            base_url: Some(config.base_url.clone()),
            max_retries: Some(0),
            ..Default::default()
        })?;
        let tools = vec![json!({
            "type": "web_search_20250305",
            "name": "web_search",
        })];

        Ok(Self {
            config,
            client,
            tools,
        })
    }

    fn request(&self, input: &[Message]) -> anyhow::Result<MessageCreateParams> {
        let (system, messages) = conversation(input)?;
        let mut request =
            MessageCreateParams::new(self.config.model.clone(), MAX_OUTPUT_TOKENS, messages)
                .stream(true);

        request.system = (!system.is_empty()).then_some(MessageContent::Blocks(system));
        request.tools = (!self.tools.is_empty()).then_some(self.tools.clone());
        request.tool_choice = Some(json!({ "type": "auto" }));
        request.thinking = Some(thinking(self.config.reasoning_effort));

        Ok(request)
    }

    fn estimate(&self, input: &[Message], include_tools: bool) -> anyhow::Result<usize> {
        let mut request = serde_json::to_value(self.request(input)?)?;

        if !include_tools && let Some(object) = request.as_object_mut() {
            object.remove("tools");
            object.remove("tool_choice");
        }

        self.estimate_value(request, input)
    }

    fn estimate_value(&self, mut value: Value, input: &[Message]) -> anyhow::Result<usize> {
        let image_count = redact_images(&mut value);

        let serialized = serde_json::to_string(&value)?;
        let tokenizer =
            bpe_for_model(&self.config.model).unwrap_or_else(|_| o200k_base_singleton());
        let text_tokens = tokenizer.count_ordinary(&serialized);
        let image_tokens = input
            .iter()
            .filter_map(|message| match message {
                Message::User(input) => Some(input.images()),
                _ => None,
            })
            .flatten()
            .map(|image| image.estimated_tokens())
            .sum::<usize>();

        Ok(text_tokens
            .saturating_sub(image_count.saturating_mul(UNKNOWN_IMAGE_TOKENS))
            .saturating_add(image_tokens))
    }

    fn compact_request(&self, input: &[Message]) -> anyhow::Result<MessageCreateParams> {
        let (_, messages) = conversation(input)?;
        let mut request = MessageCreateParams::new(
            self.config.model.clone(),
            MAX_COMPACT_OUTPUT_TOKENS,
            messages,
        );

        request.system = Some(MessageContent::Blocks(vec![ContentBlockParam::text(
            COMPACT_PROMPT,
        )]));
        request.thinking = Some(json!({ "type": "disabled" }));

        Ok(request)
    }

    fn compaction(text: &str) -> anyhow::Result<Vec<u8>> {
        if text.trim().is_empty() {
            anyhow::bail!("the compaction model returned no text");
        }

        let summary = format!(
            "{SUMMARY_OPEN}\n{}\n{SUMMARY_CLOSE}\n\n{SUMMARY_CONTINUE}",
            text.trim()
        );
        let state = StoredCompaction {
            protocol: "anthropic".to_owned(),
            messages: vec![MessageParam {
                role: Role::User,
                content: MessageContent::Blocks(vec![ContentBlockParam::text(summary)]),
            }],
        };

        Ok(serde_json::to_vec(&state)?)
    }

    fn compile_tool(spec: ToolDefinition) -> anyhow::Result<Value> {
        let input_schema = super::schema::sanitize(&spec.arguments)?;

        Ok(json!({
            "name": spec.name,
            "description": spec.description,
            "input_schema": input_schema,
        }))
    }
}

#[async_trait::async_trait]
impl Provider for AnthropicProvider {
    fn model(&self) -> &str {
        &self.config.model
    }

    fn identity(&self) -> Option<Identity> {
        Some(Identity {
            protocol: Protocol::Anthropic,
            base_url: self.config.base_url.clone(),
        })
    }

    fn thinking_effort(&self) -> Option<&str> {
        Some(effort_name(self.config.reasoning_effort))
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
            provider = "anthropic",
            tool_count
        );

        Ok(())
    }

    fn estimate_request_tokens(&self, input: &[Message]) -> anyhow::Result<Option<usize>> {
        self.estimate(input, true).map(Some)
    }

    fn estimate_output_tokens(&self, output: &[Message]) -> anyhow::Result<Option<usize>> {
        self.estimate(output, false).map(Some)
    }

    async fn compact(&self, input: &[Message]) -> anyhow::Result<Option<Compaction>> {
        if input.is_empty() {
            return Ok(None);
        }

        let request = self.compact_request(input)?;
        let input_tokens = self.estimate_value(serde_json::to_value(&request)?, input)?;
        let response = match self.client.messages().create(request).await? {
            MessageCreateResult::Message(message) => message,
            MessageCreateResult::Stream(_) => {
                anyhow::bail!("Anthropic returned a streaming compaction response")
            }
        };
        let text = response
            .content
            .iter()
            .filter_map(|block| block.text())
            .collect::<Vec<_>>()
            .join("\n");
        let tokenizer =
            bpe_for_model(&self.config.model).unwrap_or_else(|_| o200k_base_singleton());
        let output_tokens = tokenizer.count_ordinary(&text);
        let state = Self::compaction(&text)?;
        let total_tokens = input_tokens.saturating_add(output_tokens);

        tracing::info!(
            event = "provider.context.compacted",
            provider = "anthropic",
            method = "model",
            state_bytes = state.len(),
            estimated_input_tokens = input_tokens,
            estimated_output_tokens = output_tokens,
            estimated_total_tokens = total_tokens,
        );

        Ok(Some(Compaction::new(state, Some(total_tokens))))
    }

    async fn stream(&self, input: &[Message]) -> anyhow::Result<ProviderEventStream> {
        let (message_count, tool_count) = (input.len(), self.tools.len());
        let request = self.request(input)?;
        let events = match self.client.messages().create(request).await? {
            MessageCreateResult::Stream(events) => events,
            MessageCreateResult::Message(_) => {
                anyhow::bail!("Anthropic returned a non-streaming response")
            }
        };
        let mut parser = EventParser::default();
        let stream = events
            .map(move |event| match event {
                Ok(event) => parser.push(event),
                Err(error) => Err(error.into()),
            })
            .flat_map(|result| {
                let signals = match result {
                    Ok(signals) => signals.into_iter().map(Ok).collect(),
                    Err(error) => vec![Err(error)],
                };

                stream::iter(signals)
            })
            .boxed();

        tracing::info!(
            event = "provider.stream.opened",
            provider = "anthropic",
            message_count,
            tool_count
        );

        Ok(stream)
    }
}

fn conversation(input: &[Message]) -> anyhow::Result<(Vec<ContentBlockParam>, Vec<MessageParam>)> {
    let mut system = Vec::new();
    let mut messages = Vec::new();

    for message in input {
        match message {
            Message::System(text) => system.push(ContentBlockParam::text(text)),
            Message::User(input) => append(&mut messages, Role::User, user_blocks(input)),
            Message::Assistant(text) => append(
                &mut messages,
                Role::Assistant,
                vec![ContentBlockParam::text(text)],
            ),
            Message::Reasoning(state) => append(
                &mut messages,
                Role::Assistant,
                vec![native_block(state, "reasoning")?],
            ),
            Message::Search(search) => append(
                &mut messages,
                Role::Assistant,
                native_search_blocks(search)?,
            ),
            Message::Compaction(compaction) => {
                let stored = serde_json::from_slice::<StoredCompaction>(compaction.state())?;
                if stored.protocol != "anthropic" {
                    anyhow::bail!("stored compaction belongs to another provider")
                }

                for message in stored.messages {
                    let MessageContent::Blocks(blocks) = message.content else {
                        anyhow::bail!("stored Anthropic compaction must use content blocks")
                    };

                    append(&mut messages, message.role, blocks);
                }
            }
            Message::ToolCall {
                call_id,
                name,
                arguments,
            } => {
                let input = serde_json::from_str::<Value>(arguments)?;
                append(
                    &mut messages,
                    Role::Assistant,
                    vec![block(
                        "tool_use",
                        json!({
                            "id": call_id,
                            "name": name,
                            "input": input,
                        }),
                    )],
                );
            }
            Message::ToolCallResult {
                call_id, output, ..
            } => append(
                &mut messages,
                Role::User,
                vec![block(
                    "tool_result",
                    json!({
                        "tool_use_id": call_id,
                        "content": output,
                    }),
                )],
            ),
        }
    }

    Ok((system, messages))
}

fn append(messages: &mut Vec<MessageParam>, role: Role, blocks: Vec<ContentBlockParam>) {
    if blocks.is_empty() {
        return;
    }

    if let Some(last) = messages.last_mut()
        && last.role == role
        && let MessageContent::Blocks(content) = &mut last.content
    {
        content.extend(blocks);
        return;
    }

    messages.push(MessageParam {
        role,
        content: MessageContent::Blocks(blocks),
    });
}

fn user_blocks(input: &UserInput) -> Vec<ContentBlockParam> {
    let mut blocks = Vec::with_capacity(input.parts().len() * 2);
    let mut image_index = 0_usize;

    for part in input.parts() {
        match part {
            InputPart::Text(text) if !text.is_empty() => {
                blocks.push(ContentBlockParam::text(text));
            }
            InputPart::Text(_) => {}
            InputPart::Image(image) => {
                image_index += 1;
                blocks.push(ContentBlockParam::text(format!("[Image {image_index}]")));
                blocks.push(block(
                    "image",
                    json!({
                        "source": {
                            "type": "base64",
                            "media_type": image.media_type(),
                            "data": image.encoded(),
                        },
                    }),
                ));
            }
        }
    }

    blocks
}

fn native_block(state: &[u8], kind: &str) -> anyhow::Result<ContentBlockParam> {
    let block = serde_json::from_slice::<ContentBlockParam>(state)?;
    let supported = match kind {
        "reasoning" => matches!(block.block_type.as_str(), "thinking" | "redacted_thinking"),
        _ => false,
    };

    if !supported {
        anyhow::bail!("stored {kind} state belongs to another provider")
    }

    Ok(block)
}

fn native_search_blocks(search: &Search) -> anyhow::Result<Vec<ContentBlockParam>> {
    let blocks = serde_json::from_slice::<Vec<ContentBlockParam>>(search.state())?;
    let valid = matches!(
        blocks.as_slice(),
        [request, result]
            if request.block_type == "server_tool_use"
                && result.block_type == "web_search_tool_result"
    );

    if !valid {
        anyhow::bail!("stored search state belongs to another provider")
    }

    Ok(blocks)
}

fn block(block_type: &str, fields: Value) -> ContentBlockParam {
    ContentBlockParam {
        block_type: block_type.to_owned(),
        fields,
    }
}

fn redact_images(value: &mut Value) -> usize {
    match value {
        Value::Array(values) => values.iter_mut().map(redact_images).sum(),
        Value::Object(object) => {
            let is_image = object.get("type").and_then(Value::as_str) == Some("image");

            if is_image
                && let Some(data) = object
                    .get_mut("source")
                    .and_then(Value::as_object_mut)
                    .and_then(|source| source.get_mut("data"))
            {
                *data = Value::String("[image data]".to_owned());
            }

            usize::from(is_image) + object.values_mut().map(redact_images).sum::<usize>()
        }
        _ => 0,
    }
}

#[derive(Default)]
struct EventParser {
    blocks: HashMap<usize, BlockState>,
    searches: HashMap<String, Value>,
    stop_reason: Option<String>,
    saw_tool_call: bool,
}

struct BlockState {
    block: Value,
    input_json: String,
}

impl EventParser {
    fn push(&mut self, event: RawMessageStreamEvent) -> anyhow::Result<Vec<ProviderSignal>> {
        match event.event_type.as_str() {
            "content_block_start" => self.start(event.fields),
            "content_block_delta" => self.delta(event.fields),
            "content_block_stop" => self.stop(event.fields),
            "message_delta" => {
                self.stop_reason = event
                    .fields
                    .get("delta")
                    .and_then(|delta| delta.get("stop_reason"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);

                Ok(Vec::new())
            }
            "message_stop" => {
                let need_call =
                    self.stop_reason.as_deref() == Some("tool_use") || self.saw_tool_call;

                Ok(vec![ProviderSignal::Completed {
                    reason: if need_call {
                        CompletedReason::NeedCall
                    } else {
                        CompletedReason::Final
                    },
                }])
            }
            "error" => {
                let error = event.fields.get("error").unwrap_or(&event.fields);
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Anthropic stream error");

                anyhow::bail!(message.to_owned())
            }
            _ => Ok(Vec::new()),
        }
    }

    fn start(&mut self, fields: Value) -> anyhow::Result<Vec<ProviderSignal>> {
        let index = index(&fields);
        let content = fields
            .get("content_block")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("content_block_start is missing content_block"))?;
        let text = content
            .get("text")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(|text| ProviderSignal::TextDelta(text.to_owned()));

        self.blocks.insert(
            index,
            BlockState {
                block: content,
                input_json: String::new(),
            },
        );

        Ok(text.into_iter().collect())
    }

    fn delta(&mut self, fields: Value) -> anyhow::Result<Vec<ProviderSignal>> {
        let (index, delta) = (
            index(&fields),
            fields
                .get("delta")
                .ok_or_else(|| anyhow::anyhow!("content_block_delta is missing delta"))?,
        );
        let kind = delta
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(state) = self.blocks.get_mut(&index) else {
            anyhow::bail!("content block delta references unknown index {index}")
        };

        match kind {
            "text_delta" => {
                let text = delta
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                append_field(&mut state.block, "text", text);

                Ok(vec![ProviderSignal::TextDelta(text.to_owned())])
            }
            "thinking_delta" => {
                let thinking = delta
                    .get("thinking")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                append_field(&mut state.block, "thinking", thinking);

                Ok(Vec::new())
            }
            "signature_delta" => {
                let signature = delta
                    .get("signature")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                append_field(&mut state.block, "signature", signature);

                Ok(Vec::new())
            }
            "input_json_delta" => {
                let partial = delta
                    .get("partial_json")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                state.input_json.push_str(partial);

                Ok(Vec::new())
            }
            _ => Ok(Vec::new()),
        }
    }

    fn stop(&mut self, fields: Value) -> anyhow::Result<Vec<ProviderSignal>> {
        let index = index(&fields);
        let Some(mut state) = self.blocks.remove(&index) else {
            anyhow::bail!("content block stop references unknown index {index}")
        };
        let kind = state
            .block
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();

        if !state.input_json.trim().is_empty() {
            let input = serde_json::from_str::<Value>(&state.input_json)?;
            object_mut(&mut state.block)?.insert("input".to_owned(), input);
        }

        match kind.as_str() {
            "thinking" | "redacted_thinking" => Ok(vec![ProviderSignal::Reasoning(
                serde_json::to_vec(&state.block)?,
            )]),
            "tool_use" => {
                let (id, name) = (
                    required_str(&state.block, "id")?,
                    required_str(&state.block, "name")?,
                );
                let input = state
                    .block
                    .get("input")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                self.saw_tool_call = true;

                tracing::info!(
                    event = "provider.tool_call.requested",
                    provider = "anthropic",
                    tool_name = name
                );

                Ok(vec![ProviderSignal::ToolCallStarted(ToolCall::new(
                    id, name, input,
                ))])
            }
            "server_tool_use" => self.start_search(state.block),
            "web_search_tool_result" => self.finish_search(state.block),
            _ => Ok(Vec::new()),
        }
    }

    fn start_search(&mut self, block: Value) -> anyhow::Result<Vec<ProviderSignal>> {
        let (id, name) = (required_str(&block, "id")?, required_str(&block, "name")?);
        if name != "web_search" {
            return Ok(Vec::new());
        }

        let action = search_action(&block);
        let state = serde_json::to_vec(&vec![block.clone()])?;
        self.searches.insert(id.clone(), block);

        Ok(vec![ProviderSignal::Search(Search::new(
            id,
            SearchStatus::Running,
            action,
            state,
        ))])
    }

    fn finish_search(&mut self, result: Value) -> anyhow::Result<Vec<ProviderSignal>> {
        let id = required_str(&result, "tool_use_id")?;
        let Some(request) = self.searches.remove(&id) else {
            anyhow::bail!("web search result references unknown tool use {id}")
        };
        let status = if result.get("is_error").and_then(Value::as_bool) == Some(true)
            || result
                .get("content")
                .and_then(|content| content.get("type"))
                .and_then(Value::as_str)
                == Some("web_search_tool_result_error")
        {
            SearchStatus::Failed
        } else {
            SearchStatus::Succeeded
        };
        let action = search_action(&request).map(|action| match action {
            SearchAction::Query { query, .. } => SearchAction::Query {
                query,
                sources: search_sources(&result),
            },
            action => action,
        });
        let state = serde_json::to_vec(&vec![request, result])?;

        Ok(vec![ProviderSignal::Search(Search::new(
            id, status, action, state,
        ))])
    }
}

fn index(fields: &Value) -> usize {
    fields.get("index").and_then(Value::as_u64).unwrap_or(0) as usize
}

fn required_str(value: &Value, key: &str) -> anyhow::Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("content block is missing {key}"))
}

fn object_mut(value: &mut Value) -> anyhow::Result<&mut Map<String, Value>> {
    value
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("content block is not an object"))
}

fn append_field(value: &mut Value, key: &str, addition: &str) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let current = object.get(key).and_then(Value::as_str).unwrap_or_default();
    let combined = format!("{current}{addition}");

    object.insert(key.to_owned(), Value::String(combined));
}

fn search_action(block: &Value) -> Option<SearchAction> {
    let input = block.get("input")?;
    let query = input
        .get("query")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            let queries = input.get("queries")?.as_array()?;
            let queries = queries.iter().filter_map(Value::as_str).collect::<Vec<_>>();

            (!queries.is_empty()).then(|| queries.join(" | "))
        })?;

    Some(SearchAction::Query {
        query,
        sources: Vec::new(),
    })
}

fn search_sources(block: &Value) -> Vec<SearchSource> {
    block
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|result| {
            let url = result.get("url").and_then(Value::as_str)?;
            let title = result
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_owned);

            Some(SearchSource::new(url, title))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(event_type: &str, fields: Value) -> RawMessageStreamEvent {
        RawMessageStreamEvent {
            event_type: event_type.to_owned(),
            fields,
        }
    }

    fn start(index: usize, block: Value) -> RawMessageStreamEvent {
        event(
            "content_block_start",
            json!({
                "index": index,
                "content_block": block,
            }),
        )
    }

    fn delta(index: usize, delta: Value) -> RawMessageStreamEvent {
        event(
            "content_block_delta",
            json!({
                "index": index,
                "delta": delta,
            }),
        )
    }

    fn stop(index: usize) -> RawMessageStreamEvent {
        event("content_block_stop", json!({ "index": index }))
    }

    #[test]
    fn conversation_preserves_anthropic_reasoning_and_tool_order() {
        let reasoning = json!({
            "type": "thinking",
            "thinking": "inspect both files",
            "signature": "signed",
        });
        let messages = vec![
            Message::System("system prompt".to_owned()),
            Message::User("inspect".into()),
            Message::Reasoning(serde_json::to_vec(&reasoning).unwrap()),
            Message::Assistant("I will inspect both files.".to_owned()),
            Message::ToolCall {
                call_id: "call-1".to_owned(),
                name: "read_file".to_owned(),
                arguments: r#"{"path":"src/main.rs"}"#.to_owned(),
            },
            Message::ToolCall {
                call_id: "call-2".to_owned(),
                name: "read_file".to_owned(),
                arguments: r#"{"path":"src/lib.rs"}"#.to_owned(),
            },
            Message::ToolCallResult {
                call_id: "call-1".to_owned(),
                output: "main result".to_owned(),
                summary: None,
            },
            Message::ToolCallResult {
                call_id: "call-2".to_owned(),
                output: "lib result".to_owned(),
                summary: None,
            },
        ];

        let (system, conversation) = conversation(&messages).unwrap();

        assert_eq!(system, [ContentBlockParam::text("system prompt")]);
        assert_eq!(conversation.len(), 3);
        assert_eq!(conversation[0].role, Role::User);
        assert_eq!(conversation[1].role, Role::Assistant);
        assert_eq!(conversation[2].role, Role::User);

        let MessageContent::Blocks(assistant) = &conversation[1].content else {
            panic!("assistant content should use blocks");
        };
        assert_eq!(
            assistant
                .iter()
                .map(|block| block.block_type.as_str())
                .collect::<Vec<_>>(),
            ["thinking", "text", "tool_use", "tool_use"]
        );
        assert_eq!(assistant[0].fields["signature"], "signed");
        assert_eq!(assistant[2].fields["id"], "call-1");
        assert_eq!(assistant[3].fields["id"], "call-2");

        let MessageContent::Blocks(results) = &conversation[2].content else {
            panic!("tool results should use blocks");
        };
        assert_eq!(results[0].fields["tool_use_id"], "call-1");
        assert_eq!(results[1].fields["tool_use_id"], "call-2");
    }

    #[test]
    fn thinking_deltas_are_saved_with_their_signature() {
        let mut parser = EventParser::default();

        assert!(
            parser
                .push(start(
                    0,
                    json!({
                        "type": "thinking",
                        "thinking": "",
                        "signature": "",
                    }),
                ))
                .unwrap()
                .is_empty()
        );
        parser
            .push(delta(
                0,
                json!({ "type": "thinking_delta", "thinking": "inspect" }),
            ))
            .unwrap();
        parser
            .push(delta(
                0,
                json!({ "type": "signature_delta", "signature": "signed" }),
            ))
            .unwrap();

        let signals = parser.push(stop(0)).unwrap();
        let [ProviderSignal::Reasoning(state)] = signals.as_slice() else {
            panic!("expected one reasoning signal");
        };
        let block = serde_json::from_slice::<Value>(state).unwrap();

        assert_eq!(block["type"], "thinking");
        assert_eq!(block["thinking"], "inspect");
        assert_eq!(block["signature"], "signed");
    }

    #[test]
    fn tool_json_deltas_emit_one_complete_call() {
        let mut parser = EventParser::default();

        parser
            .push(start(
                1,
                json!({
                    "type": "tool_use",
                    "id": "call-1",
                    "name": "read_file",
                    "input": {},
                }),
            ))
            .unwrap();
        parser
            .push(delta(
                1,
                json!({
                    "type": "input_json_delta",
                    "partial_json": "{\"path\":",
                }),
            ))
            .unwrap();
        parser
            .push(delta(
                1,
                json!({
                    "type": "input_json_delta",
                    "partial_json": "\"src/main.rs\"}",
                }),
            ))
            .unwrap();

        let signals = parser.push(stop(1)).unwrap();
        let [ProviderSignal::ToolCallStarted(call)] = signals.as_slice() else {
            panic!("expected one tool call");
        };

        assert_eq!(call.id().as_str(), "call-1");
        assert_eq!(call.name(), "read_file");
        assert_eq!(call.arguments(), &json!({ "path": "src/main.rs" }));
    }

    #[test]
    fn search_blocks_emit_running_and_completed_searches() {
        let mut parser = EventParser::default();
        let request = json!({
            "type": "server_tool_use",
            "id": "search-1",
            "name": "web_search",
            "input": {
                "queries": ["Rust streams", "Anthropic SSE"],
            },
        });
        let result = json!({
            "type": "web_search_tool_result",
            "tool_use_id": "search-1",
            "content": [
                {
                    "type": "web_search_result",
                    "url": "https://example.com/rust",
                    "title": "Rust streams",
                },
                {
                    "type": "web_search_result",
                    "url": "https://example.com/sse",
                    "title": "SSE",
                }
            ],
        });

        parser.push(start(2, request.clone())).unwrap();
        let running = parser.push(stop(2)).unwrap();
        parser.push(start(3, result.clone())).unwrap();
        let completed = parser.push(stop(3)).unwrap();

        let [ProviderSignal::Search(running)] = running.as_slice() else {
            panic!("expected a running search");
        };
        assert_eq!(running.status(), SearchStatus::Running);

        let [ProviderSignal::Search(completed)] = completed.as_slice() else {
            panic!("expected a completed search");
        };
        assert_eq!(completed.status(), SearchStatus::Succeeded);
        assert!(matches!(
            completed.action(),
            Some(SearchAction::Query { query, sources })
                if query == "Rust streams | Anthropic SSE"
                    && sources.len() == 2
                    && sources[0].url() == "https://example.com/rust"
        ));
        assert_eq!(
            serde_json::from_slice::<Vec<Value>>(completed.state()).unwrap(),
            [request, result]
        );
    }

    #[test]
    fn message_stop_uses_the_anthropic_stop_reason() {
        let mut parser = EventParser::default();
        parser
            .push(event(
                "message_delta",
                json!({ "delta": { "stop_reason": "tool_use" } }),
            ))
            .unwrap();

        let signals = parser.push(event("message_stop", json!({}))).unwrap();

        assert!(matches!(
            signals.as_slice(),
            [ProviderSignal::Completed {
                reason: CompletedReason::NeedCall,
            }]
        ));
    }

    #[test]
    fn error_events_surface_the_upstream_message() {
        let mut parser = EventParser::default();
        let error = parser
            .push(event(
                "error",
                json!({
                    "error": {
                        "type": "overloaded_error",
                        "message": "server overloaded",
                    }
                }),
            ))
            .unwrap_err();

        assert_eq!(error.to_string(), "server overloaded");
    }

    #[test]
    fn thinking_configuration_tracks_reasoning_effort() {
        assert_eq!(
            thinking(ReasoningEffort::None),
            json!({ "type": "disabled" })
        );
        assert_eq!(thinking(ReasoningEffort::Medium)["budget_tokens"], 4_096);
        assert_eq!(thinking(ReasoningEffort::Max)["budget_tokens"], 16_384);
    }

    #[test]
    fn tool_schemas_are_sanitized_before_registration() {
        let tool = AnthropicProvider::compile_tool(ToolDefinition {
            name: "get_outgoing_calls".to_owned(),
            description: "Get outgoing calls.".to_owned(),
            arguments: json!({
                "type": "object",
                "properties": {
                    "item": {
                        "description": "The call hierarchy item."
                    }
                },
                "required": ["item"]
            }),
        })
        .unwrap();

        assert_eq!(tool["input_schema"]["properties"]["item"]["type"], "object");
    }

    #[test]
    fn compacted_state_replays_as_a_marked_user_message() {
        let state = AnthropicProvider::compaction("Continue implementing the provider.").unwrap();
        let messages = vec![
            Message::System("system prompt".to_owned()),
            Message::Compaction(Compaction::new(state, Some(42))),
            Message::Assistant("Continuing now.".to_owned()),
        ];

        let (system, conversation) = conversation(&messages).unwrap();

        assert_eq!(system, [ContentBlockParam::text("system prompt")]);
        assert_eq!(conversation.len(), 2);
        assert_eq!(conversation[0].role, Role::User);
        assert_eq!(conversation[1].role, Role::Assistant);
        let MessageContent::Blocks(summary) = &conversation[0].content else {
            panic!("compaction should use content blocks");
        };
        assert!(
            summary[0].fields["text"]
                .as_str()
                .unwrap()
                .contains("<context_summary>\nContinue implementing the provider.")
        );
    }
}
