use grep_regex::RegexMatcher;
use grep_searcher::{Searcher, SearcherBuilder, Sink};
use ignore::WalkBuilder;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    DisplayBlock, Presentation, Presenter, Summary, ToolCall, ToolCallOutcome, ToolCallResult,
    ToolCallStatus, ToolOutput, TypedTool,
    output::{Limits, save_and_preview},
};

const DEFAULT_CONTEXT_LINES: usize = 0;
const SUMMARY_VERSION: u32 = 1;

pub struct GrepTool;

#[derive(Deserialize)]
struct GrepSummary {
    path: String,
    pattern: String,
    returned_lines: usize,
    #[serde(default)]
    output_path: Option<String>,
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
    returned_lines: usize,
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

        self.output.push_str(&format!("{line_num}:{line}"));
        self.returned_lines += 1;
        Ok(true)
    }

    fn context(
        &mut self,
        _searcher: &Searcher,
        ctx: &grep_searcher::SinkContext<'_>,
    ) -> Result<bool, Self::Error> {
        let line_num = ctx.line_number().unwrap_or(0);
        let line = std::str::from_utf8(ctx.bytes()).unwrap_or("");

        self.output.push_str(&format!("{line_num}-{line}"));
        self.returned_lines += 1;
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

    async fn call(&self, arguments: Self::Arguments) -> anyhow::Result<ToolOutput<Self::Output>> {
        let matcher = RegexMatcher::new(&arguments.pattern)?;

        let mut searcher = SearcherBuilder::new()
            .before_context(arguments.before())
            .after_context(arguments.after())
            .passthru(false)
            .build();

        let (mut results, mut returned_lines) = (String::new(), 0);
        for result in WalkBuilder::new(&arguments.path).build() {
            let entry = result?;

            if entry
                .file_type()
                .is_some_and(|file_type| file_type.is_file())
            {
                let path = entry.path();
                let mut sink = GrepSink {
                    output: String::new(),
                    returned_lines: 0,
                };

                searcher.search_path(&matcher, path, &mut sink)?;

                if sink.output.is_empty() {
                    continue;
                }
                if !results.is_empty() {
                    if !results.ends_with('\n') {
                        results.push('\n');
                    }

                    results.push('\n');
                }

                results.push_str(&path.display().to_string());
                results.push('\n');
                results.push_str(&sink.output);
                returned_lines += sink.returned_lines;
            }
        }

        let preview = save_and_preview(&results, "grep", Limits::DEFAULT).await?;
        let output_path = preview.path.clone();
        let output = GrepToolOutput {
            results: preview.content,
        };
        let summary = Summary::new(
            SUMMARY_VERSION,
            serde_json::json!({
                "path": arguments.path,
                "pattern": arguments.pattern,
                "returned_lines": returned_lines,
                "output_path": output_path,
            }),
        );

        Ok(ToolOutput::new(output).with_summary(summary))
    }

    fn compact(&self, summary: &Summary) -> anyhow::Result<Option<String>> {
        let summary = summary.deserialize::<GrepSummary>(SUMMARY_VERSION)?;
        let mut detail = format!(
            "Matched {} lines in {:?} for pattern {:?}.",
            summary.returned_lines, summary.path, summary.pattern
        );

        if let Some(path) = summary.output_path {
            detail.push_str(&format!(" Full output: {path}."));
        }

        Ok(Some(detail))
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
        Presentation {
            call_id: call.id.clone(),
            name: "Grep".to_owned(),
            label: "built-in".to_owned(),
            target: Self::target(call),
            status: ToolCallStatus::Running,
            blocks: Vec::new(),
        }
    }

    fn completed(&self, call: &ToolCall, result: &ToolCallResult) -> Presentation {
        let (status, blocks) = match &result.outcome {
            ToolCallOutcome::Success(_) => (ToolCallStatus::Succeeded, Vec::new()),
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
