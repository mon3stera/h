use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::interaction::{AskAnswer, AskOption, AskQuestion, Bridge};

use super::{
    Presentation, Presenter, ToolCall, ToolCallOutcome, ToolCallResult, ToolCallStatus, ToolOutput,
    TypedTool,
};

pub struct AskTool {
    bridge: Bridge,
}

impl AskTool {
    pub fn new(bridge: Bridge) -> Self {
        Self { bridge }
    }
}

#[derive(Clone, Deserialize, JsonSchema)]
pub struct AskToolArgs {
    /// question to put to the user
    question: String,
    /// options to offer; the user can always answer with something else instead
    options: Vec<AskToolOption>,
}

#[derive(Clone, Deserialize, JsonSchema)]
pub struct AskToolOption {
    /// short text naming the option
    label: String,
    /// what choosing this option means
    description: Option<String>,
}

#[derive(Serialize)]
pub struct AskToolOutput {
    /// label the user picked, or the text they wrote themselves
    answer: String,
    /// true when the user wrote their own answer rather than picking an option
    free_text: bool,
    /// index of the option that was picked, absent for a written answer
    option_index: Option<usize>,
}

#[async_trait::async_trait]
impl TypedTool for AskTool {
    type Arguments = AskToolArgs;
    type Output = AskToolOutput;

    fn name(&self) -> &'static str {
        "ask"
    }

    fn description(&self) -> &'static str {
        "Ask the user to choose between options when the decision is theirs to make. \
         Blocks until they answer, so use it for decisions that change what you do next, \
         not for questions you can resolve yourself."
    }

    async fn call(&self, arguments: Self::Arguments) -> anyhow::Result<ToolOutput<Self::Output>> {
        if arguments.options.is_empty() {
            anyhow::bail!("ask requires at least one option");
        }

        let question = AskQuestion {
            question: arguments.question,
            options: arguments
                .options
                .into_iter()
                .map(|option| AskOption {
                    label: option.label,
                    description: option.description,
                })
                .collect(),
        };

        let output = match self.bridge.ask(question).await? {
            AskAnswer::Option { index, label } => AskToolOutput {
                answer: label,
                free_text: false,
                option_index: Some(index),
            },
            AskAnswer::FreeText(text) => AskToolOutput {
                answer: text,
                free_text: true,
                option_index: None,
            },
        };

        Ok(ToolOutput::new(output))
    }
}

/// Budgets set by what is left of an eighty-column line once the title's own
/// furniture — the indicator, the name, the label — has taken its share.
pub(super) const MAX_QUESTION_CHARS: usize = 48;
pub(super) const MAX_ANSWER_CHARS: usize = 24;

/// Shortens for a title, where every column counts.
///
/// The block helpers spell out `… [truncated]`, which is right in a body and far
/// too heavy on a single line.
fn clip(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }

    text.chars().take(max_chars).chain(['…']).collect()
}

pub struct AskPresenter;

/// Shows the exchange on the title line and nothing else.
///
/// A question and its answer are one fact, and the user just supplied the answer
/// themselves — spending rows to repeat it back would only push the conversation
/// off screen.
impl Presenter for AskPresenter {
    fn completed(&self, call: &ToolCall, result: &ToolCallResult) -> Presentation {
        let asked = question(call);

        let (status, target) = match &result.outcome {
            ToolCallOutcome::Success(output) => {
                let answer = output
                    .get("answer")
                    .and_then(Value::as_str)
                    .unwrap_or_default();

                (
                    ToolCallStatus::Succeeded,
                    format!("{asked} → {}", clip(answer, MAX_ANSWER_CHARS)),
                )
            }
            // Dismissed rather than answered; the reason rides on the status.
            ToolCallOutcome::Failure { message } => (
                ToolCallStatus::Failed {
                    message: message.clone(),
                },
                asked,
            ),
        };

        Presentation {
            call_id: call.id.clone(),
            name: "Ask".to_owned(),
            label: "built-in".to_owned(),
            target: Some(target),
            status,
            blocks: Vec::new(),
        }
    }

    fn running(&self, call: &ToolCall) -> Presentation {
        Presentation {
            call_id: call.id.clone(),
            name: "Ask".to_owned(),
            label: "built-in".to_owned(),
            target: Some(question(call)),
            status: ToolCallStatus::Running,
            blocks: Vec::new(),
        }
    }
}

fn question(call: &ToolCall) -> String {
    clip(
        call.arguments
            .get("question")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        MAX_QUESTION_CHARS,
    )
}
