use std::io;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use similar::TextDiff;
use strsim::normalized_levenshtein;
use tokio::fs;

use super::TypedTool;

pub struct EditTool;

#[derive(Deserialize, JsonSchema)]
pub struct EditToolArgs {
    /// path of a file
    path: String,
    /// source that will be replaced from
    source: String,
    /// target that will be replaced into
    target: String,
}

#[derive(Serialize)]
struct ExactMatchCandidates {
    start_line: usize,
    end_line: usize,
}

#[derive(Serialize)]
enum EditStatus {
    Ok,
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

    async fn call(&self, arguments: Self::Arguments) -> anyhow::Result<Self::Output> {
        let mut content = match fs::read_to_string(&arguments.path).await {
            Ok(content) => content,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(EditToolOutput {
                    status: EditStatus::FileNotFound,
                    applied: false,
                });
            }
            Err(error) => anyhow::bail!("{error}"),
        };

        if content.contains(&arguments.source) {
            content = content.replace(&arguments.source, &arguments.target);

            fs::write(arguments.path, content).await?;

            return Ok(EditToolOutput {
                status: EditStatus::Ok,
                applied: true,
            });
        }

        let content_lines = content.lines().collect::<Vec<_>>();
        let source_lines = arguments.source.lines().collect::<Vec<_>>();
        let (content_line_num, source_line_num) = (content_lines.len(), source_lines.len());
        let mut matches = Vec::new();

        if content_line_num < source_line_num {
            return Ok(EditToolOutput {
                status: EditStatus::InvalidRange {
                    message: "File content length is less than source's".to_owned(),
                },
                applied: false,
            });
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
            return Ok(EditToolOutput {
                status: EditStatus::SimilarMatches { matches },
                applied: false,
            });
        }

        Ok(EditToolOutput {
            status: EditStatus::NoCandidate {
                message: "There is no candidate that is exact to or similar to the source"
                    .to_owned(),
            },
            applied: false,
        })
    }
}
