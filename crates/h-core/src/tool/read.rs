use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{
    fs::{self, File},
    io::{AsyncBufReadExt, AsyncSeekExt, BufReader},
};

use super::{
    DisplayBlock, Presentation, Presenter, Summary, ToolCall, ToolCallOutcome, ToolCallResult,
    ToolCallStatus, ToolOutput, TypedTool,
    file_buffer::{FileBufferStore, FileFingerprint, IndexedFile, is_cacheable},
};

pub(super) const MAX_READ_LINES: usize = 500;
pub(super) const MAX_READ_CHARS: usize = 2_048;
const SUMMARY_VERSION: u32 = 1;

#[derive(Clone, Deserialize, JsonSchema)]
pub struct ReadFileToolArgs {
    /// File path.
    pub(super) path: String,
    /// First line to read. Line numbers are 1-based and inclusive. Defaults to 1.
    pub(super) start_line: Option<usize>,
    /// Last line to read. Line numbers are 1-based and inclusive. If omitted, reads up to 500 lines. Ranges longer than 500 lines are clamped to 500.
    pub(super) end_line: Option<usize>,
    /// Zero-based byte offset within start_line. Defaults to 0. Use next_start_line and next_offset from the previous result to continue.
    pub(super) offset: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct ReadFileToolOutput {
    pub(super) content: String,
    pub(super) start_line: usize,
    pub(super) end_line: Option<usize>,
    pub(super) total_lines: Option<usize>,
    pub(super) has_more: bool,
    pub(super) offset: usize,
    pub(super) next_start_line: Option<usize>,
    pub(super) next_offset: Option<usize>,
    pub(super) truncated_lines: usize,
    pub(super) truncated_bytes: usize,
}

pub struct ReadFileTool {
    buffers: FileBufferStore,
}

#[derive(Deserialize)]
struct ReadSummary {
    path: String,
    lines: usize,
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
        offset: usize,
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
                offset,
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
            offset,
        )
        .await
    }
}

async fn read_indexed_range(
    file: File,
    index: &mut IndexedFile,
    start_line: usize,
    requested_end: usize,
    offset: usize,
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
            offset: 0,
            next_start_line: None,
            next_offset: None,
            truncated_lines: 0,
            truncated_bytes: 0,
        });
    }

    let actual_end = requested_end.min(available_lines);
    let source = read_lines_from_offsets(&mut reader, index, start_line, actual_end).await?;
    let first_line_bytes = source.find('\n').unwrap_or(source.len());
    anyhow::ensure!(
        offset <= first_line_bytes,
        "offset exceeds the length of start_line"
    );
    anyhow::ensure!(
        source.is_char_boundary(offset),
        "offset must be on a UTF-8 character boundary"
    );

    let page_end = offset + char_prefix_len(&source[offset..], MAX_READ_CHARS);
    let content = source[offset..page_end].to_owned();
    let more_lines = match total_lines {
        Some(total_lines) => actual_end < total_lines,
        None => index.line_starts.len() > actual_end,
    };
    let (next_start_line, next_offset) = if page_end < source.len() {
        let prefix = &source[..page_end];
        let next_start_line = start_line + prefix.bytes().filter(|byte| *byte == b'\n').count();
        let next_offset = page_end
            - prefix
                .rfind('\n')
                .map_or(0, |newline| newline.saturating_add(1));

        (Some(next_start_line), Some(next_offset))
    } else if more_lines {
        (Some(actual_end.saturating_add(1)), Some(0))
    } else {
        (None, None)
    };
    let end_line = if content.is_empty() {
        None
    } else {
        let newlines = content.bytes().filter(|byte| *byte == b'\n').count();
        let lines = newlines + usize::from(!content.ends_with('\n'));

        Some(start_line.saturating_add(lines.saturating_sub(1)))
    };
    let truncated_lines = next_start_line.map_or(0, |next_line| {
        if next_line > actual_end {
            0
        } else {
            actual_end - next_line + 1
        }
    });

    Ok(ReadFileToolOutput {
        content,
        start_line,
        end_line,
        total_lines,
        has_more: next_start_line.is_some(),
        offset,
        next_start_line,
        next_offset,
        truncated_lines,
        truncated_bytes: source.len().saturating_sub(page_end),
    })
}

fn char_prefix_len(content: &str, max_chars: usize) -> usize {
    content
        .char_indices()
        .nth(max_chars)
        .map_or(content.len(), |(index, _)| index)
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
        "read a file page with at most 500 lines and 2048 characters; line ranges are 1-based and inclusive, offset is a zero-based byte position within start_line, and next_start_line/next_offset continue truncated output"
    }

    async fn call(&self, arguments: Self::Arguments) -> anyhow::Result<ToolOutput<Self::Output>> {
        let start_line = arguments.start_line.unwrap_or(1);
        anyhow::ensure!(start_line > 0, "start_line must be at least 1");

        if let Some(end_line) = arguments.end_line {
            anyhow::ensure!(
                end_line >= start_line,
                "end_line must be greater than or equal to start_line"
            );
        }

        let max_end = start_line.saturating_add(MAX_READ_LINES - 1);
        let requested_end = arguments.end_line.unwrap_or(max_end).min(max_end);
        let offset = arguments.offset.unwrap_or(0);
        let output = self
            .read_range(
                Path::new(&arguments.path),
                start_line,
                requested_end,
                offset,
            )
            .await?;
        let lines = output.end_line.map_or(0, |end_line| {
            end_line
                .checked_sub(output.start_line)
                .and_then(|distance| distance.checked_add(1))
                .unwrap_or(0)
        });
        let summary = Summary::new(
            SUMMARY_VERSION,
            serde_json::json!({
                "path": arguments.path,
                "lines": lines,
            }),
        );

        Ok(ToolOutput::new(output).with_summary(summary))
    }

    fn compact(&self, summary: &Summary) -> anyhow::Result<Option<String>> {
        let summary = summary.deserialize::<ReadSummary>(SUMMARY_VERSION)?;

        Ok(Some(format!(
            "Read {} lines from {:?}.",
            summary.lines, summary.path
        )))
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
            ToolCallOutcome::Success(_) => (ToolCallStatus::Succeeded, Vec::new()),
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
