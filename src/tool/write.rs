use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{
    fs::{self, OpenOptions},
    io::AsyncWriteExt,
};

use super::{
    DisplayBlock, Presentation, Presenter, ToolCall, ToolCallOutcome, ToolCallResult,
    ToolCallStatus, TypedTool, file_buffer::FileBufferStore,
};

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WriteFileMode {
    Overwrite,
    Append,
}

fn default_write_file_mode() -> WriteFileMode {
    WriteFileMode::Overwrite
}

#[derive(Clone, Deserialize, JsonSchema)]
pub struct WriteFileToolArgs {
    /// File path.
    pub(super) path: String,
    /// Content to write.
    pub(super) content: String,
    /// Write mode. `overwrite` replaces the file; `append` adds content to the end. Defaults to `overwrite`.
    #[serde(default = "default_write_file_mode")]
    pub(super) mode: WriteFileMode,
}

#[derive(Serialize)]
pub struct WriteFileToolOutput {
    pub(super) status: String,
}

pub struct WriteFileTool {
    buffers: FileBufferStore,
}

impl WriteFileTool {
    pub fn new(buffers: FileBufferStore) -> Self {
        Self { buffers }
    }
}

#[async_trait::async_trait]
impl TypedTool for WriteFileTool {
    type Arguments = WriteFileToolArgs;
    type Output = WriteFileToolOutput;

    fn name(&self) -> &'static str {
        "write_file"
    }

    fn description(&self) -> &'static str {
        "write content to a file by overwriting it or appending to its end"
    }

    async fn call(&self, arguments: Self::Arguments) -> anyhow::Result<Self::Output> {
        let path = PathBuf::from(&arguments.path);

        match arguments.mode {
            WriteFileMode::Overwrite => fs::write(&path, arguments.content).await?,
            WriteFileMode::Append => {
                let mut file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .await?;
                file.write_all(arguments.content.as_bytes()).await?;
                file.flush().await?;
            }
        }

        self.buffers.invalidate(&path).await;

        Ok(WriteFileToolOutput {
            status: "Ok".to_owned(),
        })
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
        let mode = call
            .arguments
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("overwrite");
        let action = if mode == "append" {
            "Appended"
        } else {
            "Wrote"
        };

        let (status, blocks) = match &result.outcome {
            ToolCallOutcome::Success(_) => (
                ToolCallStatus::Succeeded,
                vec![
                    DisplayBlock::Summary(format!("{action} {lines_cnt} lines")),
                    DisplayBlock::CodeBlock {
                        language: Some("raw".to_owned()),
                        content: content.to_owned(),
                        truncated_lines: 10,
                        show_line_numbers: true,
                        start_line_number: 1,
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
