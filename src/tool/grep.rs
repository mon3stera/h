use std::fmt::Write as _;

use grep_regex::RegexMatcher;
use grep_searcher::{Searcher, SearcherBuilder, Sink};
use ignore::WalkBuilder;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    Aggregator, DisplayBlock, Presentation, Presenter, Summary, ToolCall, ToolCallOutcome,
    ToolCallResult, ToolCallStatus, TypedTool, presentation::truncate_preview, summary::Targets,
};

const DEFAULT_CONTEXT_LINES: usize = 0;
const SUMMARY_VERSION: u32 = 1;

pub struct GrepTool;

#[derive(Deserialize)]
struct GrepSummary {
    path: String,
    pattern: String,
    returned_lines: usize,
}

#[derive(Default)]
struct GrepAggregator {
    paths: Targets,
    patterns: Targets,
    returned_lines: usize,
}

impl Aggregator for GrepAggregator {
    fn push(&mut self, summary: &Summary) -> anyhow::Result<()> {
        let summary = summary.deserialize::<GrepSummary>(SUMMARY_VERSION)?;

        self.paths.push(&summary.path);
        self.patterns.push(&summary.pattern);
        self.returned_lines = self.returned_lines.saturating_add(summary.returned_lines);
        Ok(())
    }

    fn finish(self: Box<Self>, buf: &mut String) {
        buf.push_str("\n- Grep paths: ");
        self.paths.write_description(buf, "path");
        buf.push_str("; patterns: ");
        self.patterns.write_description(buf, "pattern");
        let _ = write!(buf, "; returned_lines: {}", self.returned_lines);
    }
}

#[derive(Clone, Deserialize, JsonSchema)]
pub struct GrepToolArgs {
    /// file or directory
    pub(super) path: String,
    /// regex
    pub(super) pattern: String,
    /// Include N lines before each match. Defaults to 0.
    pub(super) before: Option<usize>,
    /// Include N lines after each match. Defaults to 0.
    pub(super) after: Option<usize>,
}

impl GrepToolArgs {
    pub(super) fn before(&self) -> usize {
        self.before.unwrap_or(DEFAULT_CONTEXT_LINES)
    }

    pub(super) fn after(&self) -> usize {
        self.after.unwrap_or(DEFAULT_CONTEXT_LINES)
    }
}

#[derive(Serialize)]
pub struct GrepToolOutput {
    pub(super) results: String,
}

struct GrepSink {
    output: String,
    path: String,
}

impl Sink for GrepSink {
    type Error = std::io::Error;

    fn matched(
        &mut self,
        _searcher: &Searcher,
        mat: &grep_searcher::SinkMatch<'_>,
    ) -> Result<bool, Self::Error> {
        let line_num = mat.line_number().unwrap_or(0);
        let line = std::str::from_utf8(mat.bytes()).unwrap_or("");

        self.output
            .push_str(&format!("{}:{}:{}", self.path, line_num, line));
        Ok(true)
    }

    fn context(
        &mut self,
        _searcher: &Searcher,
        ctx: &grep_searcher::SinkContext<'_>,
    ) -> Result<bool, Self::Error> {
        let line_num = ctx.line_number().unwrap_or(0);
        let line = std::str::from_utf8(ctx.bytes()).unwrap_or("");

        self.output
            .push_str(&format!("{}-{}-{}", self.path, line_num, line));
        Ok(true)
    }

    fn context_break(&mut self, _searcher: &Searcher) -> Result<bool, Self::Error> {
        self.output.push_str("--\n");
        Ok(true)
    }
}

#[async_trait::async_trait]
impl TypedTool for GrepTool {
    type Arguments = GrepToolArgs;
    type Output = GrepToolOutput;

    fn name(&self) -> &'static str {
        "grep"
    }

    fn description(&self) -> &'static str {
        "grep a pattern in specific path (file or directory)"
    }

    async fn call(&self, arguments: Self::Arguments) -> anyhow::Result<Self::Output> {
        let matcher = RegexMatcher::new(&arguments.pattern)?;

        let mut searcher = SearcherBuilder::new()
            .before_context(arguments.before())
            .after_context(arguments.after())
            .passthru(false)
            .build();

        let mut results = String::new();
        for result in WalkBuilder::new(&arguments.path).build() {
            let entry = result?;

            if entry
                .file_type()
                .is_some_and(|file_type| file_type.is_file())
            {
                let path = entry.path();
                let mut sink = GrepSink {
                    output: String::new(),
                    path: path.display().to_string(),
                };

                searcher.search_path(&matcher, path, &mut sink)?;

                results.push('\n');
                results.push_str(&sink.output);
            }
        }

        Ok(GrepToolOutput { results })
    }

    fn summarize(&self, arguments: &Self::Arguments, output: &Self::Output) -> Option<Summary> {
        let returned_lines = output
            .results
            .trim_matches('\n')
            .lines()
            .filter(|line| !line.is_empty() && *line != "--")
            .count();

        Some(Summary::new(
            SUMMARY_VERSION,
            serde_json::json!({
                "path": arguments.path,
                "pattern": arguments.pattern,
                "returned_lines": returned_lines,
            }),
        ))
    }

    fn aggregator(&self) -> Option<Box<dyn Aggregator>> {
        Some(Box::new(GrepAggregator::default()))
    }
}

pub struct GrepPresenter;

impl GrepPresenter {
    fn target(call: &ToolCall) -> Option<String> {
        call.arguments
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }
}

impl Presenter for GrepPresenter {
    fn running(&self, call: &ToolCall) -> Presentation {
        let pattern = call
            .arguments
            .get("pattern")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let blocks = if pattern.is_empty() {
            Vec::new()
        } else {
            vec![DisplayBlock::Summary(format!("Searching for {pattern:?}"))]
        };

        Presentation {
            call_id: call.id.clone(),
            name: "Grep".to_owned(),
            label: "built-in".to_owned(),
            target: Self::target(call),
            status: ToolCallStatus::Running,
            blocks,
        }
    }

    fn completed(&self, call: &ToolCall, result: &ToolCallResult) -> Presentation {
        let (status, blocks) = match &result.outcome {
            ToolCallOutcome::Success(output) => {
                let results = output
                    .get("results")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim_matches('\n');

                if results.is_empty() {
                    (
                        ToolCallStatus::Succeeded,
                        vec![DisplayBlock::Summary("No matches".to_owned())],
                    )
                } else {
                    let returned_lines = results
                        .lines()
                        .filter(|line| !line.is_empty() && *line != "--")
                        .count();
                    let (content, truncated_lines) = truncate_preview(results);

                    (
                        ToolCallStatus::Succeeded,
                        vec![
                            DisplayBlock::Summary(format!("Returned {returned_lines} lines")),
                            DisplayBlock::TextOutput {
                                content,
                                truncated_lines,
                            },
                        ],
                    )
                }
            }
            ToolCallOutcome::Failure { message } => (
                ToolCallStatus::Failed {
                    message: message.clone(),
                },
                vec![DisplayBlock::Summary("Grep failed".to_owned())],
            ),
        };

        Presentation {
            call_id: call.id.clone(),
            name: "Grep".to_owned(),
            label: "built-in".to_owned(),
            target: Self::target(call),
            status,
            blocks,
        }
    }
}
