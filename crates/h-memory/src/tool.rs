use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use h_core::tool::{
    DisplayBlock, Presentation, Presenter, Summary, ToolCall, ToolCallOutcome, ToolCallResult,
    ToolCallStatus, ToolOutput, TypedTool,
};

use crate::{Draft, Scope, Store};

const MAX_READ_LINES: usize = 500;
const MAX_READ_CHARS: usize = 16_384;
const DEFAULT_SEARCH_RESULTS: usize = 10;
const SUMMARY_VERSION: u32 = 1;

#[derive(Clone)]
pub struct ReadTool {
    store: Store,
}

#[derive(Clone, Deserialize, JsonSchema)]
pub struct ReadArgs {
    /// Memory scope. Defaults to the current project.
    #[serde(default)]
    scope: Scope,
    /// Stable topic id returned by search_memory or listed in the memory index.
    id: String,
    /// First content line to read. Line numbers are 1-based and inclusive. Defaults to 1.
    start_line: Option<usize>,
    /// Last content line to read. Ranges longer than 500 lines are clamped to 500.
    end_line: Option<usize>,
    /// Zero-based byte offset within start_line. Use the returned continuation values to resume.
    offset: Option<usize>,
}

#[derive(Serialize)]
pub struct ReadOutput {
    scope: Scope,
    id: String,
    title: String,
    summary: String,
    keywords: Vec<String>,
    content: String,
    revision: String,
    start_line: usize,
    end_line: Option<usize>,
    total_lines: usize,
    has_more: bool,
    offset: usize,
    next_start_line: Option<usize>,
    next_offset: Option<usize>,
    truncated_lines: usize,
    truncated_bytes: usize,
}

#[derive(Deserialize)]
struct ReadSummary {
    scope: Scope,
    id: String,
}

impl ReadTool {
    pub fn new(store: Store) -> Self {
        Self { store }
    }
}

#[async_trait::async_trait]
impl TypedTool for ReadTool {
    type Arguments = ReadArgs;
    type Output = ReadOutput;

    fn name(&self) -> &'static str {
        "read_memory"
    }

    fn description(&self) -> &'static str {
        "Read one persistent memory topic by scope and id. Output is limited to 500 lines and 16384 characters; use next_start_line and next_offset to continue. Read an existing topic before replacing it with write_memory."
    }

    async fn call(&self, args: Self::Arguments) -> anyhow::Result<ToolOutput<Self::Output>> {
        let start_line = args.start_line.unwrap_or(1);
        let entry = self.store.read(args.scope, &args.id).await?;
        let page = page(
            &entry.content,
            start_line,
            args.end_line,
            args.offset.unwrap_or(0),
        )?;
        let output = ReadOutput {
            scope: entry.scope,
            id: entry.id.clone(),
            title: entry.title,
            summary: entry.summary,
            keywords: entry.keywords,
            content: page.content,
            revision: entry.revision,
            start_line: page.start_line,
            end_line: page.end_line,
            total_lines: page.total_lines,
            has_more: page.next_start_line.is_some(),
            offset: page.offset,
            next_start_line: page.next_start_line,
            next_offset: page.next_offset,
            truncated_lines: page.truncated_lines,
            truncated_bytes: page.truncated_bytes,
        };
        let summary = Summary::new(
            SUMMARY_VERSION,
            serde_json::json!({
                "scope": args.scope,
                "id": entry.id,
            }),
        );

        Ok(ToolOutput::new(output).with_summary(summary))
    }

    fn compact(&self, summary: &Summary) -> anyhow::Result<Option<String>> {
        let summary = summary.deserialize::<ReadSummary>(SUMMARY_VERSION)?;

        Ok(Some(format!(
            "Read {} memory {:?}.",
            summary.scope.label(),
            summary.id
        )))
    }
}

#[derive(Clone)]
pub struct SearchTool {
    store: Store,
}

#[derive(Clone, Deserialize, JsonSchema)]
pub struct SearchArgs {
    /// Terms that must all occur in a topic's id, title, summary, keywords, or content.
    query: String,
    /// Restrict search to user or current-project memory. Searches both when omitted.
    scope: Option<Scope>,
    /// Maximum number of results. Defaults to 10 and is clamped to 50.
    limit: Option<usize>,
}

#[derive(Serialize)]
pub struct SearchOutput {
    query: String,
    results: Vec<crate::Hit>,
}

#[derive(Deserialize)]
struct SearchSummary {
    query: String,
    results: usize,
}

impl SearchTool {
    pub fn new(store: Store) -> Self {
        Self { store }
    }
}

#[async_trait::async_trait]
impl TypedTool for SearchTool {
    type Arguments = SearchArgs;
    type Output = SearchOutput;

    fn name(&self) -> &'static str {
        "search_memory"
    }

    fn description(&self) -> &'static str {
        "Search all persistent memory topics in user and current-project scope, including topics omitted from the startup index snapshot. Use it when previous knowledge may be relevant but no visible index entry matches."
    }

    async fn call(&self, args: Self::Arguments) -> anyhow::Result<ToolOutput<Self::Output>> {
        let results = self
            .store
            .search(
                &args.query,
                args.scope,
                args.limit.unwrap_or(DEFAULT_SEARCH_RESULTS),
            )
            .await?;
        let summary = Summary::new(
            SUMMARY_VERSION,
            serde_json::json!({
                "query": args.query,
                "results": results.len(),
            }),
        );
        let output = SearchOutput {
            query: args.query,
            results,
        };

        Ok(ToolOutput::new(output).with_summary(summary))
    }

    fn compact(&self, summary: &Summary) -> anyhow::Result<Option<String>> {
        let summary = summary.deserialize::<SearchSummary>(SUMMARY_VERSION)?;

        Ok(Some(format!(
            "Found {} memory topics for {:?}.",
            summary.results, summary.query
        )))
    }
}

#[derive(Clone)]
pub struct WriteTool {
    store: Store,
}

#[derive(Clone, Deserialize, JsonSchema)]
pub struct WriteArgs {
    /// Memory scope. Defaults to the current project. Use user only for cross-project preferences or workflows.
    #[serde(default)]
    scope: Scope,
    /// Stable lowercase slug containing only letters, digits, and single hyphens.
    id: String,
    /// Short human-readable topic title.
    title: String,
    /// One-line retrieval hint, at most 200 characters.
    summary: String,
    /// Search terms that are not already obvious from the title and summary.
    #[serde(default)]
    keywords: Vec<String>,
    /// Complete Markdown body for the topic. Merge existing and new facts before replacing a topic.
    content: String,
    /// Revision returned by read_memory. Required when replacing an existing topic and omitted when creating one.
    expected_revision: Option<String>,
}

#[derive(Serialize)]
pub struct WriteOutput {
    scope: Scope,
    id: String,
    path: String,
    revision: String,
    created: bool,
}

#[derive(Deserialize)]
struct WriteSummary {
    scope: Scope,
    id: String,
    created: bool,
}

impl WriteTool {
    pub fn new(store: Store) -> Self {
        Self { store }
    }
}

#[async_trait::async_trait]
impl TypedTool for WriteTool {
    type Arguments = WriteArgs;
    type Output = WriteOutput;

    fn name(&self) -> &'static str {
        "write_memory"
    }

    fn description(&self) -> &'static str {
        "Create or replace a persistent memory topic. Record only stable information likely to help future sessions, never secrets or transient progress. Writes default to the current project. Read an existing topic first and pass expected_revision when updating it."
    }

    async fn call(&self, args: Self::Arguments) -> anyhow::Result<ToolOutput<Self::Output>> {
        let result = self
            .store
            .write(
                args.scope,
                Draft {
                    id: args.id,
                    title: args.title,
                    summary: args.summary,
                    keywords: args.keywords,
                    content: args.content,
                    expected_revision: args.expected_revision,
                },
            )
            .await?;
        let output = WriteOutput {
            scope: result.entry.scope,
            id: result.entry.id.clone(),
            path: result.entry.path.display().to_string(),
            revision: result.entry.revision,
            created: result.created,
        };
        let summary = Summary::new(
            SUMMARY_VERSION,
            serde_json::json!({
                "scope": output.scope,
                "id": output.id,
                "created": output.created,
            }),
        );

        Ok(ToolOutput::new(output).with_summary(summary))
    }

    fn compact(&self, summary: &Summary) -> anyhow::Result<Option<String>> {
        let summary = summary.deserialize::<WriteSummary>(SUMMARY_VERSION)?;
        let action = if summary.created {
            "Created"
        } else {
            "Updated"
        };

        Ok(Some(format!(
            "{action} {} memory {:?}.",
            summary.scope.label(),
            summary.id
        )))
    }
}

pub struct ReadPresenter;
pub struct SearchPresenter;
pub struct WritePresenter;

impl Presenter for ReadPresenter {
    fn running(&self, call: &ToolCall) -> Presentation {
        presentation(
            call,
            "ReadMemory",
            topic_target(call),
            ToolCallStatus::Running,
            Vec::new(),
        )
    }

    fn completed(&self, call: &ToolCall, result: &ToolCallResult) -> Presentation {
        completed(
            call,
            result,
            "ReadMemory",
            topic_target(call),
            "Failed to read memory",
        )
    }
}

impl Presenter for SearchPresenter {
    fn running(&self, call: &ToolCall) -> Presentation {
        presentation(
            call,
            "SearchMemory",
            query_target(call),
            ToolCallStatus::Running,
            Vec::new(),
        )
    }

    fn completed(&self, call: &ToolCall, result: &ToolCallResult) -> Presentation {
        completed(
            call,
            result,
            "SearchMemory",
            query_target(call),
            "Failed to search memory",
        )
    }
}

impl Presenter for WritePresenter {
    fn running(&self, call: &ToolCall) -> Presentation {
        presentation(
            call,
            "WriteMemory",
            topic_target(call),
            ToolCallStatus::Running,
            Vec::new(),
        )
    }

    fn completed(&self, call: &ToolCall, result: &ToolCallResult) -> Presentation {
        completed(
            call,
            result,
            "WriteMemory",
            topic_target(call),
            "Failed to write memory",
        )
    }
}

fn completed(
    call: &ToolCall,
    result: &ToolCallResult,
    name: &str,
    target: Option<String>,
    failure: &str,
) -> Presentation {
    let (status, blocks) = match result.outcome() {
        ToolCallOutcome::Success(_) => (ToolCallStatus::Succeeded, Vec::new()),
        ToolCallOutcome::Failure { message } => (
            ToolCallStatus::Failed {
                message: message.clone(),
            },
            vec![DisplayBlock::Summary(failure.to_owned())],
        ),
    };

    presentation(call, name, target, status, blocks)
}

fn presentation(
    call: &ToolCall,
    name: &str,
    target: Option<String>,
    status: ToolCallStatus,
    blocks: Vec<DisplayBlock>,
) -> Presentation {
    Presentation {
        call_id: call.id().clone(),
        name: name.to_owned(),
        label: "memory".to_owned(),
        target,
        status,
        blocks,
    }
}

fn topic_target(call: &ToolCall) -> Option<String> {
    let scope = call
        .arguments()
        .get("scope")
        .and_then(Value::as_str)
        .unwrap_or("project");
    let id = call.arguments().get("id").and_then(Value::as_str)?;

    Some(format!("{scope}/{id}"))
}

fn query_target(call: &ToolCall) -> Option<String> {
    call.arguments()
        .get("query")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

#[derive(Debug)]
struct Page {
    content: String,
    start_line: usize,
    end_line: Option<usize>,
    total_lines: usize,
    offset: usize,
    next_start_line: Option<usize>,
    next_offset: Option<usize>,
    truncated_lines: usize,
    truncated_bytes: usize,
}

fn page(
    content: &str,
    start_line: usize,
    end_line: Option<usize>,
    offset: usize,
) -> anyhow::Result<Page> {
    anyhow::ensure!(start_line > 0, "start_line must be at least 1");

    if let Some(end_line) = end_line {
        anyhow::ensure!(
            end_line >= start_line,
            "end_line must be greater than or equal to start_line"
        );
    }

    let lines = content.lines().collect::<Vec<_>>();
    let total_lines = lines.len();

    if start_line > total_lines {
        anyhow::ensure!(offset == 0, "offset requires an existing start_line");

        return Ok(Page {
            content: String::new(),
            start_line,
            end_line: None,
            total_lines,
            offset: 0,
            next_start_line: None,
            next_offset: None,
            truncated_lines: 0,
            truncated_bytes: 0,
        });
    }

    let max_end = start_line.saturating_add(MAX_READ_LINES - 1);
    let requested_end = end_line.unwrap_or(total_lines).min(total_lines);
    let capped_end = requested_end.min(max_end);
    let source = lines[start_line - 1..capped_end].join("\n");
    let requested_source = lines[start_line - 1..requested_end].join("\n");
    let first_line_bytes = lines[start_line - 1].len();

    anyhow::ensure!(
        offset <= first_line_bytes,
        "offset exceeds the length of start_line"
    );
    anyhow::ensure!(
        source.is_char_boundary(offset),
        "offset must be on a UTF-8 character boundary"
    );

    let page_end = offset + char_prefix_len(&source[offset..], MAX_READ_CHARS);
    let page_content = source[offset..page_end].to_owned();
    let (next_start_line, next_offset) = if page_end < source.len() {
        let prefix = &source[..page_end];
        let next_start_line = start_line + prefix.bytes().filter(|byte| *byte == b'\n').count();
        let next_offset = page_end
            - prefix
                .rfind('\n')
                .map_or(0, |newline| newline.saturating_add(1));

        (Some(next_start_line), Some(next_offset))
    } else if capped_end < requested_end {
        (Some(capped_end.saturating_add(1)), Some(0))
    } else {
        (None, None)
    };
    let end_line = if page_end == source.len() {
        Some(capped_end)
    } else {
        let newlines = page_content.bytes().filter(|byte| *byte == b'\n').count();
        let last_line = start_line
            .saturating_add(newlines)
            .saturating_sub(usize::from(page_content.ends_with('\n')));

        Some(last_line)
    };
    let truncated_lines = next_start_line.map_or(0, |next_line| {
        if next_line > requested_end {
            0
        } else {
            requested_end - next_line + 1
        }
    });
    let truncated_bytes = requested_source
        .len()
        .saturating_sub(offset)
        .saturating_sub(page_content.len());

    Ok(Page {
        content: page_content,
        start_line,
        end_line,
        total_lines,
        offset,
        next_start_line,
        next_offset,
        truncated_lines,
        truncated_bytes,
    })
}

fn char_prefix_len(content: &str, max_chars: usize) -> usize {
    content
        .char_indices()
        .nth(max_chars)
        .map_or(content.len(), |(index, _)| index)
}

#[cfg(test)]
mod tests {
    use h_core::tool::{Presenter, ToolCall, ToolCallStatus};
    use serde_json::json;

    use super::*;

    #[test]
    fn write_defaults_to_project_scope() {
        let args = serde_json::from_value::<WriteArgs>(json!({
            "id": "architecture",
            "title": "Architecture",
            "summary": "Current architecture decisions.",
            "content": "# Architecture"
        }))
        .unwrap();

        assert_eq!(args.scope, Scope::Project);
    }

    #[test]
    fn read_page_continues_inside_a_long_utf8_line() {
        let content = "界".repeat(MAX_READ_CHARS + 10);
        let first = page(&content, 1, None, 0).unwrap();
        let second = page(
            &content,
            first.next_start_line.unwrap(),
            None,
            first.next_offset.unwrap(),
        )
        .unwrap();

        assert_eq!(first.content.chars().count(), MAX_READ_CHARS);
        assert_eq!(first.truncated_bytes, 30);
        assert_eq!(first.truncated_lines, 1);
        assert_eq!(second.content.chars().count(), 10);
        assert_eq!(first.next_start_line, Some(1));
        assert!(!second.content.is_empty());
    }

    #[test]
    fn explicit_read_range_does_not_continue_past_its_end() {
        let content = (1..=10)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let page = page(&content, 3, Some(4), 0).unwrap();

        assert_eq!(page.content, "line 3\nline 4");
        assert_eq!(page.end_line, Some(4));
        assert_eq!(page.next_start_line, None);
        assert_eq!(page.truncated_lines, 0);
        assert_eq!(page.truncated_bytes, 0);
    }

    #[test]
    fn read_page_reports_content_omitted_by_the_line_limit() {
        let content = vec!["x"; MAX_READ_LINES + 2].join("\n");
        let page = page(&content, 1, None, 0).unwrap();

        assert_eq!(page.next_start_line, Some(MAX_READ_LINES + 1));
        assert_eq!(page.next_offset, Some(0));
        assert_eq!(page.truncated_lines, 2);
        assert!(page.truncated_bytes > 0);
    }

    #[test]
    fn read_rejects_an_offset_inside_a_utf8_character() {
        let error = page("界", 1, None, 1).unwrap_err();

        assert!(error.to_string().contains("UTF-8 character boundary"));
    }

    #[test]
    fn memory_presenters_do_not_render_tool_output_bodies() {
        let call = ToolCall::new(
            "call-1",
            "search_memory",
            json!({ "query": "architecture" }),
        );
        let presentation = SearchPresenter.running(&call);

        assert!(matches!(presentation.status, ToolCallStatus::Running));
        assert_eq!(presentation.target.as_deref(), Some("architecture"));
        assert!(presentation.blocks.is_empty());
    }
}
