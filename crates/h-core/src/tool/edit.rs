use std::io;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use similar::{ChangeTag, TextDiff};
use strsim::normalized_levenshtein;
use tokio::fs;

use super::{
    DiffLine, DiffLineKind, DisplayBlock, Presentation, Presenter, ToolCall, ToolCallOutcome,
    ToolCallResult, ToolCallStatus, ToolOutput, TypedTool,
};

pub struct EditTool;

#[derive(Clone, Deserialize, JsonSchema)]
pub struct EditToolArgs {
    /// path of a file
    pub(super) path: String,
    /// source that will be replaced from
    pub(super) source: String,
    /// target that will be replaced into
    pub(super) target: String,
}

#[derive(Serialize)]
struct ExactMatchCandidates {
    start_line: usize,
    end_line: usize,
}

/// Unchanged file lines kept on each side of an edit, so the change can be read
/// in place rather than in isolation.
const CONTEXT_LINES: usize = 3;

#[derive(Serialize)]
enum EditStatus {
    /// Everything a reader needs to see the edit against the real file: the line
    /// the replacement landed on, and the untouched lines around it.
    Ok {
        start_line: usize,
        context_before: Vec<String>,
        context_after: Vec<String>,
    },
    MultipleExactMatches {
        candidates: Vec<ExactMatchCandidates>,
    },
    NoCandidate {
        message: String,
    },
    SimilarMatches {
        matches: Vec<MatchResult>,
    },
    FileNotFound,
    InvalidRange {
        message: String,
    },
}

#[derive(Serialize)]
struct MatchResult {
    similarity: f64,
    start: usize,
    end: usize,
    actual_source: String,
    diff: String,
}

#[derive(Serialize)]
pub struct EditToolOutput {
    status: EditStatus,
    applied: bool,
}

#[async_trait::async_trait]
impl TypedTool for EditTool {
    type Arguments = EditToolArgs;
    type Output = EditToolOutput;

    fn name(&self) -> &'static str {
        "edit"
    }

    fn description(&self) -> &'static str {
        "Edit a file"
    }

    async fn call(&self, arguments: Self::Arguments) -> anyhow::Result<ToolOutput<Self::Output>> {
        let mut content = match fs::read_to_string(&arguments.path).await {
            Ok(content) => content,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(ToolOutput::new(EditToolOutput {
                    status: EditStatus::FileNotFound,
                    applied: false,
                }));
            }
            Err(error) => anyhow::bail!("{error}"),
        };

        // An empty source matches between every character; replacing it would
        // shred the file rather than edit it.
        if arguments.source.is_empty() {
            return Ok(ToolOutput::new(EditToolOutput {
                status: EditStatus::InvalidRange {
                    message: "source must not be empty".to_owned(),
                },
                applied: false,
            }));
        }

        let exact = exact_matches(&content, &arguments.source);

        // More than one exact hit means the caller has not said which one it
        // meant. Report where they all are so the next attempt can widen its
        // source until it is unique, rather than guessing.
        if exact.len() > 1 {
            return Ok(ToolOutput::new(EditToolOutput {
                status: EditStatus::MultipleExactMatches {
                    candidates: exact.iter().map(ExactMatch::to_candidate).collect(),
                },
                applied: false,
            }));
        }

        if let Some(hit) = exact.first() {
            let (context_before, context_after) =
                surrounding_context(&content, hit.start_line, arguments.source.lines().count());

            // Replacing the one range that matched, rather than every occurrence:
            // uniqueness is established above, and this cannot reach any other.
            content.replace_range(
                hit.offset..hit.offset + arguments.source.len(),
                &arguments.target,
            );

            fs::write(arguments.path, content).await?;

            return Ok(ToolOutput::new(EditToolOutput {
                status: EditStatus::Ok {
                    start_line: hit.start_line,
                    context_before,
                    context_after,
                },
                applied: true,
            }));
        }

        let content_lines = content.lines().collect::<Vec<_>>();
        let source_lines = arguments.source.lines().collect::<Vec<_>>();
        let (content_line_num, source_line_num) = (content_lines.len(), source_lines.len());
        let mut matches = Vec::new();

        if content_line_num < source_line_num {
            return Ok(ToolOutput::new(EditToolOutput {
                status: EditStatus::InvalidRange {
                    message: "File content length is less than source's".to_owned(),
                },
                applied: false,
            }));
        }

        for window_size in [
            source_line_num + 1,
            source_line_num,
            source_line_num.saturating_sub(1),
        ] {
            if window_size > 0 {
                for i in 0..=content_line_num.saturating_sub(window_size) {
                    let segment = content_lines[i..i + window_size].join("\n");
                    let similarity = normalized_levenshtein(&segment, &arguments.source) as f64;

                    if similarity > 0.85 {
                        matches.push(MatchResult {
                            similarity,
                            start: i + 1,
                            end: i + window_size + 1,
                            actual_source: segment.clone(),
                            diff: TextDiff::from_lines(&arguments.source, segment)
                                .unified_diff()
                                .to_string(),
                        })
                    }
                }
            }
        }

        if !matches.is_empty() {
            return Ok(ToolOutput::new(EditToolOutput {
                status: EditStatus::SimilarMatches { matches },
                applied: false,
            }));
        }

        Ok(ToolOutput::new(EditToolOutput {
            status: EditStatus::NoCandidate {
                message: "There is no candidate that is exact to or similar to the source"
                    .to_owned(),
            },
            applied: false,
        }))
    }
}

pub(super) struct ExactMatch {
    offset: usize,
    start_line: usize,
    end_line: usize,
}

impl ExactMatch {
    #[cfg(test)]
    pub(super) fn start_line(&self) -> usize {
        self.start_line
    }

    #[cfg(test)]
    pub(super) fn end_line(&self) -> usize {
        self.end_line
    }

    fn to_candidate(&self) -> ExactMatchCandidates {
        ExactMatchCandidates {
            start_line: self.start_line,
            end_line: self.end_line,
        }
    }
}

/// Every non-overlapping occurrence of `source`, with the 1-based inclusive line
/// range it spans.
///
/// Matches arrive in ascending order, so the line count advances once across the
/// file instead of being recounted from the start for each hit.
pub(super) fn exact_matches(content: &str, source: &str) -> Vec<ExactMatch> {
    if source.is_empty() {
        return Vec::new();
    }

    let spanned_lines = source.lines().count().max(1);
    let mut matches = Vec::new();
    let mut cursor = 0;
    let mut line = 1;

    for (offset, _) in content.match_indices(source) {
        line += content[cursor..offset].matches('\n').count();
        cursor = offset;

        matches.push(ExactMatch {
            offset,
            start_line: line,
            end_line: line + spanned_lines - 1,
        });
    }

    matches
}

/// The untouched lines immediately before and after a replaced block, at most
/// [`CONTEXT_LINES`] on each side and fewer at the edges of the file.
///
/// `start_line` is 1-based; `replaced_lines` counts the file lines the source
/// spans.
pub(super) fn surrounding_context(
    content: &str,
    start_line: usize,
    replaced_lines: usize,
) -> (Vec<String>, Vec<String>) {
    let lines = content.lines().collect::<Vec<_>>();
    let owned = |slice: &[&str]| slice.iter().map(|line| (*line).to_owned()).collect();

    let block_start = start_line.saturating_sub(1);
    let before = lines
        .get(block_start.saturating_sub(CONTEXT_LINES)..block_start)
        .map_or_else(Vec::new, owned);

    let block_end = block_start.saturating_add(replaced_lines);
    let after = lines
        .get(block_end..block_end.saturating_add(CONTEXT_LINES).min(lines.len()))
        .map_or_else(Vec::new, owned);

    (before, after)
}

struct RenderedDiff {
    lines: Vec<DiffLine>,
    removed: usize,
    added: usize,
}

/// The replacement as a diff, framed by the untouched lines around it.
///
/// Line numbers are real file numbers, anchored at `start_line`. A removal is
/// numbered on the pre-edit side and everything else on the post-edit side, which
/// is why the trailing context resumes after the *target* block rather than the
/// source one.
///
/// Every change is kept. An edit is the one thing a reader has to be able to
/// audit in full, so nothing here is elided.
fn render_diff(
    source: &str,
    target: &str,
    start_line: usize,
    context_before: &[String],
    context_after: &[String],
) -> RenderedDiff {
    let mut lines = Vec::new();
    let (mut removed, mut added) = (0, 0);

    let leading_start = start_line.saturating_sub(context_before.len());

    for (offset, text) in context_before.iter().enumerate() {
        lines.push(DiffLine {
            number: leading_start + offset,
            kind: DiffLineKind::Context,
            text: text.clone(),
        });
    }

    for change in TextDiff::from_lines(source, target).iter_all_changes() {
        let (kind, index) = match change.tag() {
            ChangeTag::Delete => {
                removed += 1;
                (DiffLineKind::Removed, change.old_index())
            }
            ChangeTag::Insert => {
                added += 1;
                (DiffLineKind::Added, change.new_index())
            }
            ChangeTag::Equal => (DiffLineKind::Context, change.new_index()),
        };

        lines.push(DiffLine {
            number: start_line + index.unwrap_or(0),
            kind,
            text: change.value().trim_end_matches(['\r', '\n']).to_owned(),
        });
    }

    let trailing_start = start_line + target.lines().count();

    for (offset, text) in context_after.iter().enumerate() {
        lines.push(DiffLine {
            number: trailing_start + offset,
            kind: DiffLineKind::Context,
            text: text.clone(),
        });
    }

    RenderedDiff {
        lines,
        removed,
        added,
    }
}

/// Why an edit did not land, read back from the serialized [`EditStatus`].
///
/// Unit variants serialize as a bare string, the rest as a single-key object.
fn rejection(status: &Value) -> String {
    if let Some(name) = status.as_str() {
        return match name {
            "FileNotFound" => "File not found".to_owned(),
            other => other.to_owned(),
        };
    }

    let Some((name, body)) = status.as_object().and_then(|status| status.iter().next()) else {
        return "Edit was not applied".to_owned();
    };

    if let Some(message) = body.get("message").and_then(Value::as_str) {
        return message.to_owned();
    }

    let count = |key: &str| {
        body.get(key)
            .and_then(Value::as_array)
            .map_or(0, |values| values.len())
    };

    match name.as_str() {
        "MultipleExactMatches" => {
            let lines = body
                .get("candidates")
                .and_then(Value::as_array)
                .map(|candidates| {
                    candidates
                        .iter()
                        .filter_map(|candidate| candidate.get("start_line"))
                        .filter_map(Value::as_u64)
                        .map(|line| line.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();

            format!(
                "Source matches {} places exactly (lines {lines}); make it unique",
                count("candidates")
            )
        }
        "SimilarMatches" => format!(
            "Source has no exact match; {} near miss(es) found",
            count("matches")
        ),
        other => other.to_owned(),
    }
}

pub struct EditPresenter;

impl EditPresenter {
    fn argument(call: &ToolCall, key: &str) -> String {
        call.arguments
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    }
}

impl Presenter for EditPresenter {
    fn completed(&self, call: &ToolCall, result: &ToolCallResult) -> Presentation {
        let source = Self::argument(call, "source");
        let target = Self::argument(call, "target");

        let (status, blocks) = match &result.outcome {
            ToolCallOutcome::Success(output) => {
                let applied = output
                    .get("applied")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);

                if applied {
                    let landing = output.get("status").and_then(|status| status.get("Ok"));
                    let start_line = landing
                        .and_then(|landing| landing.get("start_line"))
                        .and_then(Value::as_u64)
                        .map_or(1, |line| line as usize);
                    let context = |key: &str| {
                        landing
                            .and_then(|landing| landing.get(key))
                            .and_then(Value::as_array)
                            .map(|lines| {
                                lines
                                    .iter()
                                    .filter_map(Value::as_str)
                                    .map(str::to_owned)
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default()
                    };

                    let diff = render_diff(
                        &source,
                        &target,
                        start_line,
                        &context("context_before"),
                        &context("context_after"),
                    );

                    (
                        ToolCallStatus::Succeeded,
                        vec![
                            DisplayBlock::Summary(format!(
                                "-{} +{} lines",
                                diff.removed, diff.added
                            )),
                            DisplayBlock::Diff { lines: diff.lines },
                        ],
                    )
                } else {
                    // The call itself succeeded while the edit did not land;
                    // presenting that as a success would mislead.
                    let message = output
                        .get("status")
                        .map_or_else(|| "Edit was not applied".to_owned(), rejection);

                    (
                        ToolCallStatus::Failed {
                            message: message.clone(),
                        },
                        vec![DisplayBlock::Summary(message)],
                    )
                }
            }
            ToolCallOutcome::Failure { message } => (
                ToolCallStatus::Failed {
                    message: message.clone(),
                },
                vec![DisplayBlock::Summary("Failed to edit file".to_owned())],
            ),
        };

        Presentation {
            call_id: call.id.clone(),
            name: "Edit".to_owned(),
            label: "built-in".to_owned(),
            target: Some(Self::argument(call, "path")),
            status,
            blocks,
        }
    }

    fn running(&self, call: &ToolCall) -> Presentation {
        Presentation {
            call_id: call.id.clone(),
            name: "Edit".to_owned(),
            label: "built-in".to_owned(),
            target: Some(Self::argument(call, "path")),
            status: ToolCallStatus::Running,
            blocks: Vec::new(),
        }
    }
}
