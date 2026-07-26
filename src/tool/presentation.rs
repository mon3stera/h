use serde_json::Value;

use super::{ToolCall, ToolCallId, ToolCallOutcome, ToolCallResult};

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
        show_line_numbers: bool,
        start_line_number: usize,
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
pub(super) const MAX_FIELD_CHARS: usize = 160;
pub(super) const MAX_ERROR_CHARS: usize = 500;
pub(super) const REDACTED: &str = "[REDACTED]";

pub(super) fn humanize_tool_name(name: &str) -> String {
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

pub(super) fn truncate_chars(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_owned();
    }

    let mut output = input.chars().take(max_chars).collect::<String>();
    output.push_str("… [truncated]");
    output
}

pub(super) fn truncate_preview(input: &str) -> (String, usize) {
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

pub(super) fn value_to_display_block(value: &Value, empty_summary: &str) -> DisplayBlock {
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
