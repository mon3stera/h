use std::{fmt::Write as _, path::Path};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{
    fs::{self, File},
    io::{AsyncBufReadExt, AsyncSeekExt, BufReader},
};

use super::{
    Aggregator, DisplayBlock, Presentation, Presenter, Summary, ToolCall, ToolCallOutcome,
    ToolCallResult, ToolCallStatus, TypedTool,
    file_buffer::{FileBufferStore, FileFingerprint, IndexedFile, is_cacheable},
    summary::Targets,
};

pub(super) const MAX_READ_LINES: usize = 200;
const SUMMARY_VERSION: u32 = 1;

#[derive(Clone, Deserialize, JsonSchema)]
pub struct ReadFileToolArgs {
    /// File path.
    pub(super) path: String,
    /// First line to read. Line numbers are 1-based and inclusive. Defaults to 1.
    pub(super) start_line: Option<usize>,
    /// Last line to read. Line numbers are 1-based and inclusive. If omitted, reads up to 200 lines.
    pub(super) end_line: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct ReadFileToolOutput {
    pub(super) content: String,
    pub(super) start_line: usize,
    pub(super) end_line: Option<usize>,
    pub(super) total_lines: Option<usize>,
    pub(super) has_more: bool,
}

pub struct ReadFileTool {
    buffers: FileBufferStore,
}

#[derive(Deserialize)]
struct ReadSummary {
    path: String,
    lines: usize,
}

#[derive(Default)]
struct ReadAggregator {
    paths: Targets,
    lines: usize,
}

impl Aggregator for ReadAggregator {
    fn push(&mut self, summary: &Summary) -> anyhow::Result<()> {
        let summary = summary.deserialize::<ReadSummary>(SUMMARY_VERSION)?;

        self.paths.push(&summary.path);
        self.lines = self.lines.saturating_add(summary.lines);
        Ok(())
    }

    fn finish(self: Box<Self>, buf: &mut String) {
        buf.push_str("\n- Read files: ");
        self.paths.write_description(buf, "file");
        let _ = write!(buf, "; total_lines: {}", self.lines);
    }
}

impl ReadFileTool {
    pub fn new(buffers: FileBufferStore) -> Self {
        Self { buffers }
    }

    async fn read_range(
        &self,
        path: &Path,
        start_line: usize,
        requested_end: usize,
    ) -> anyhow::Result<ReadFileToolOutput> {
        let canonical_path = fs::canonicalize(path).await?;
        let metadata = fs::metadata(&canonical_path).await?;
        let fingerprint = FileFingerprint::from_metadata(&metadata);

        if !is_cacheable(&metadata) {
            let mut index = IndexedFile::new(fingerprint);
            return read_indexed_range(
                File::open(&canonical_path).await?,
                &mut index,
                start_line,
                requested_end,
            )
            .await;
        }

        let index = self
            .buffers
            .index_for(&canonical_path, fingerprint.clone())
            .await;
        let mut index = index.lock().await;
        if index.fingerprint != fingerprint {
            index.reset(fingerprint);
        }

        read_indexed_range(
            File::open(&canonical_path).await?,
            &mut index,
            start_line,
            requested_end,
        )
        .await
    }
}

async fn read_indexed_range(
    file: File,
    index: &mut IndexedFile,
    start_line: usize,
    requested_end: usize,
) -> anyhow::Result<ReadFileToolOutput> {
    let lookahead_line = requested_end.saturating_add(1);
    let mut reader = BufReader::new(file);
    extend_line_index(&mut reader, index, lookahead_line).await?;

    let total_lines = index.total_lines;
    let available_lines = total_lines.unwrap_or(index.line_starts.len());
    if start_line > available_lines {
        return Ok(ReadFileToolOutput {
            content: String::new(),
            start_line,
            end_line: None,
            total_lines,
            has_more: false,
        });
    }

    let actual_end = requested_end.min(available_lines);
    let content = read_lines_from_offsets(&mut reader, index, start_line, actual_end).await?;
    let has_more = match total_lines {
        Some(total_lines) => actual_end < total_lines,
        None => index.line_starts.len() > actual_end,
    };

    Ok(ReadFileToolOutput {
        content,
        start_line,
        end_line: Some(actual_end),
        total_lines,
        has_more,
    })
}

async fn extend_line_index(
    reader: &mut BufReader<File>,
    index: &mut IndexedFile,
    target_line: usize,
) -> anyhow::Result<()> {
    if index.total_lines.is_some() || index.line_starts.len() >= target_line {
        return Ok(());
    }

    reader
        .seek(std::io::SeekFrom::Start(index.scanned_to))
        .await?;

    loop {
        let line_start = index.scanned_to;
        let mut bytes = Vec::new();
        let bytes_read = reader.read_until(b'\n', &mut bytes).await?;

        if bytes_read == 0 {
            index.total_lines = Some(index.line_starts.len());
            return Ok(());
        }

        validate_line_bytes(&bytes)?;
        index.line_starts.push(line_start);
        index.scanned_to = index
            .scanned_to
            .checked_add(u64::try_from(bytes_read)?)
            .ok_or_else(|| anyhow::anyhow!("file offset overflow"))?;

        if index.line_starts.len() >= target_line {
            return Ok(());
        }
    }
}

async fn read_lines_from_offsets(
    reader: &mut BufReader<File>,
    index: &IndexedFile,
    start_line: usize,
    end_line: usize,
) -> anyhow::Result<String> {
    reader
        .seek(std::io::SeekFrom::Start(index.line_starts[start_line - 1]))
        .await?;

    let mut lines = Vec::with_capacity(end_line - start_line + 1);
    for _ in start_line..=end_line {
        let mut bytes = Vec::new();
        let bytes_read = reader.read_until(b'\n', &mut bytes).await?;
        anyhow::ensure!(bytes_read > 0, "file changed while it was being read");
        strip_line_ending(&mut bytes);
        lines.push(String::from_utf8(bytes)?);
    }

    Ok(lines.join("\n"))
}

fn validate_line_bytes(bytes: &[u8]) -> anyhow::Result<()> {
    let mut content = bytes;
    if content.last() == Some(&b'\n') {
        content = &content[..content.len() - 1];
        if content.last() == Some(&b'\r') {
            content = &content[..content.len() - 1];
        }
    }

    std::str::from_utf8(content)?;
    Ok(())
}

fn strip_line_ending(bytes: &mut Vec<u8>) {
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
}

#[async_trait::async_trait]
impl TypedTool for ReadFileTool {
    type Arguments = ReadFileToolArgs;
    type Output = ReadFileToolOutput;

    fn name(&self) -> &'static str {
        "read_file"
    }

    fn description(&self) -> &'static str {
        "read a 1-based inclusive range from a file; returns at most 200 lines and total_lines is null until EOF is reached"
    }

    async fn call(&self, arguments: Self::Arguments) -> anyhow::Result<Self::Output> {
        let start_line = arguments.start_line.unwrap_or(1);
        anyhow::ensure!(start_line > 0, "start_line must be at least 1");

        if let Some(end_line) = arguments.end_line {
            anyhow::ensure!(
                end_line >= start_line,
                "end_line must be greater than or equal to start_line"
            );
            let requested_lines = end_line
                .checked_sub(start_line)
                .and_then(|distance| distance.checked_add(1))
                .ok_or_else(|| anyhow::anyhow!("requested line range is too large"))?;
            anyhow::ensure!(
                requested_lines <= MAX_READ_LINES,
                "cannot read more than {MAX_READ_LINES} lines at once"
            );
        }

        let requested_end = arguments
            .end_line
            .unwrap_or_else(|| start_line.saturating_add(MAX_READ_LINES - 1));

        self.read_range(Path::new(&arguments.path), start_line, requested_end)
            .await
    }

    fn summarize(&self, arguments: &Self::Arguments, output: &Self::Output) -> Option<Summary> {
        let lines = output.end_line.map_or(0, |end_line| {
            end_line
                .checked_sub(output.start_line)
                .and_then(|distance| distance.checked_add(1))
                .unwrap_or(0)
        });

        Some(Summary::new(
            SUMMARY_VERSION,
            serde_json::json!({
                "path": arguments.path,
                "lines": lines,
            }),
        ))
    }

    fn aggregator(&self) -> Option<Box<dyn Aggregator>> {
        Some(Box::new(ReadAggregator::default()))
    }
}

pub struct ReadFilePresenter;

impl Presenter for ReadFilePresenter {
    fn completed(&self, call: &ToolCall, result: &ToolCallResult) -> Presentation {
        let path = call
            .arguments
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_owned);

        let (status, blocks) = match &result.outcome {
            ToolCallOutcome::Success(output) => {
                let content = output
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let start_line = output
                    .get("start_line")
                    .and_then(Value::as_u64)
                    .and_then(|line| usize::try_from(line).ok())
                    .unwrap_or(1);
                let end_line = output
                    .get("end_line")
                    .and_then(Value::as_u64)
                    .and_then(|line| usize::try_from(line).ok());
                let total_lines = output
                    .get("total_lines")
                    .and_then(Value::as_u64)
                    .and_then(|line| usize::try_from(line).ok());
                let has_more = output
                    .get("has_more")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);

                let summary = match (end_line, total_lines) {
                    (Some(end_line), Some(total_lines)) => {
                        format!("Read lines {start_line}–{end_line} of {total_lines}")
                    }
                    (Some(end_line), None) if has_more => format!(
                        "Read lines {start_line}–{end_line} (total unknown; more available)"
                    ),
                    (Some(end_line), None) => {
                        format!("Read lines {start_line}–{end_line} (total unknown)")
                    }
                    (None, Some(total_lines)) => {
                        format!("No lines at or after {start_line} (file has {total_lines} lines)")
                    }
                    (None, None) => format!("No lines returned at or after {start_line}"),
                };

                let mut blocks = vec![DisplayBlock::Summary(summary)];
                if end_line.is_some() {
                    blocks.push(DisplayBlock::CodeBlock {
                        language: Some("raw".to_owned()),
                        content: content.to_owned(),
                        truncated_lines: 10,
                        show_line_numbers: true,
                        start_line_number: start_line,
                    });
                }

                (ToolCallStatus::Succeeded, blocks)
            }
            ToolCallOutcome::Failure { message } => (
                ToolCallStatus::Failed {
                    message: message.clone(),
                },
                vec![DisplayBlock::Summary("Failed to read file".to_owned())],
            ),
        };

        Presentation {
            call_id: call.id.clone(),
            name: "ReadFile".to_owned(),
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
            name: "ReadFile".to_owned(),
            label: "built-in".to_owned(),
            target: path,
            status: ToolCallStatus::Running,
            blocks: Vec::new(),
        }
    }
}
