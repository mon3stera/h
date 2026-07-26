use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    bridge::UiBridge,
    event::{AskAnswer, AskOption, AskQuestion},
};

use super::TypedTool;

pub struct AskTool {
    bridge: UiBridge,
}

impl AskTool {
    pub fn new(bridge: UiBridge) -> Self {
        Self { bridge }
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct AskToolArgs {
    /// question to put to the user
    question: String,
    /// options to offer; the user can always answer with something else instead
    options: Vec<AskToolOption>,
}

#[derive(Deserialize, JsonSchema)]
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

    async fn call(&self, arguments: Self::Arguments) -> anyhow::Result<Self::Output> {
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

        Ok(match self.bridge.ask(question).await? {
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
        })
    }
}
