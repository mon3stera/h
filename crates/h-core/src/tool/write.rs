use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{
    fs::{self, OpenOptions},
    io::{AsyncReadExt, AsyncWriteExt},
};

use super::{
    DiffLine, DiffLineKind, DisplayBlock, Presentation, Presenter, ToolCall, ToolCallOutcome,
    ToolCallResult, ToolCallStatus, ToolOutput, TypedTool, file_buffer::FileBufferStore,
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
    pub(super) start_line: usize,
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
        "write content to a file by overwriting it or appending to its end, creating missing parent directories as needed"
    }

    async fn call(&self, arguments: Self::Arguments) -> anyhow::Result<ToolOutput<Self::Output>> {
        let path = PathBuf::from(&arguments.path);

        ensure_parent_dir(&path).await?;

        let start_line = match arguments.mode {
            WriteFileMode::Overwrite => {
                fs::write(&path, arguments.content).await?;

                1
            }
            WriteFileMode::Append => {
                let mut file = OpenOptions::new()
                    .create(true)
                    .read(true)
                    .append(true)
                    .open(&path)
                    .await?;
                let start_line = append_start_line(&mut file).await?;

                file.write_all(arguments.content.as_bytes()).await?;
                file.flush().await?;

                start_line
            }
        };

        self.buffers.invalidate(&path).await;

        Ok(ToolOutput::new(WriteFileToolOutput {
            status: "Ok".to_owned(),
            start_line,
        }))
    }
}

/// Create the parent directory of `path` when it is missing. Both write modes
/// already create the file itself; this covers the `NotFound` case where the
/// directory chain above it does not exist yet.
async fn ensure_parent_dir(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).await?;
    }

    Ok(())
}

async fn append_start_line(file: &mut fs::File) -> anyhow::Result<usize> {
    let mut start_line = 1_usize;
    let mut buf = [0_u8; 8 * 1024];

    loop {
        let read = file.read(&mut buf).await?;
        if read == 0 {
            break;
        }

        let newlines = buf[..read].iter().filter(|byte| **byte == b'\n').count();
        start_line = start_line.saturating_add(newlines);
    }

    Ok(start_line)
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
            ToolCallOutcome::Success(output) => {
                let start_line = output
                    .get("start_line")
                    .and_then(Value::as_u64)
                    .and_then(|line| usize::try_from(line).ok())
                    .unwrap_or(1);
                let lines = content
                    .lines()
                    .enumerate()
                    .map(|(offset, text)| DiffLine {
                        number: start_line.saturating_add(offset),
                        kind: DiffLineKind::Added,
                        text: text.to_owned(),
                    })
                    .collect();

                (
                    ToolCallStatus::Succeeded,
                    vec![
                        DisplayBlock::Summary(format!("{action} {lines_cnt} lines")),
                        DisplayBlock::Diff { lines },
                    ],
                )
            }
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
