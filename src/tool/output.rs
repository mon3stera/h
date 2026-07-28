use std::{fmt::Write as _, path::Path};

use tokio::fs;
use uuid::Uuid;

pub(super) const MAX_OUTPUT_LINES: usize = 500;
pub(super) const MAX_OUTPUT_CHARS: usize = 2_048;

#[derive(Clone, Copy, Debug)]
pub(super) struct Limits {
    lines: usize,
    chars: usize,
}

impl Limits {
    pub(super) const DEFAULT: Self = Self {
        lines: MAX_OUTPUT_LINES,
        chars: MAX_OUTPUT_CHARS,
    };

    pub(super) fn split(left: &str, right: &str) -> (Self, Self) {
        let (left_lines, right_lines) =
            split_limit(MAX_OUTPUT_LINES, line_count(left), line_count(right));
        let (left_chars, right_chars) = split_limit(
            MAX_OUTPUT_CHARS,
            left.chars().count(),
            right.chars().count(),
        );

        (
            Self {
                lines: left_lines,
                chars: left_chars,
            },
            Self {
                lines: right_lines,
                chars: right_chars,
            },
        )
    }
}

#[derive(Debug)]
pub(super) struct Preview {
    pub(super) content: String,
    pub(super) path: Option<String>,
}

pub(super) async fn save(content: &str, prefix: &str) -> anyhow::Result<Option<String>> {
    if content.is_empty() {
        return Ok(None);
    }

    let path = std::env::temp_dir().join(format!("h-{prefix}-{}.log", Uuid::new_v4().simple()));
    fs::write(&path, content).await?;
    Ok(Some(path.display().to_string()))
}

pub(super) async fn save_and_preview(
    content: &str,
    prefix: &str,
    limits: Limits,
) -> anyhow::Result<Preview> {
    if fits(content, limits) {
        return Ok(Preview {
            content: content.to_owned(),
            path: None,
        });
    }

    let path = save(content, prefix)
        .await?
        .expect("truncated output is never empty");
    Ok(preview(content, Path::new(&path), limits))
}

pub(super) fn preview(content: &str, path: &Path, limits: Limits) -> Preview {
    if fits(content, limits) {
        return Preview {
            content: content.to_owned(),
            path: None,
        };
    }

    let (head_lines, tail_lines) = (limits.lines.div_ceil(2), limits.lines / 2);
    let (head_chars, tail_chars) = (limits.chars.div_ceil(2), limits.chars / 2);
    let head_end = prefix_end(content, head_lines, head_chars);
    let tail_start = suffix_start(content, tail_lines, tail_chars).max(head_end);
    let omitted_bytes = tail_start.saturating_sub(head_end);
    let head_boundary = head_end == 0 || content.as_bytes().get(head_end - 1) == Some(&b'\n');
    let tail_boundary = tail_start == 0 || content.as_bytes().get(tail_start - 1) == Some(&b'\n');
    let start_line = line_at(content, head_end);
    let end_line = if tail_boundary {
        line_at(content, tail_start).saturating_sub(1)
    } else {
        line_at(content, tail_start)
    }
    .max(start_line);
    let mut output = content[..head_end].to_owned();

    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }

    if head_boundary && tail_boundary {
        if start_line == end_line {
            let _ = writeln!(
                output,
                "[line {start_line} omitted; {omitted_bytes} bytes omitted]"
            );
        } else {
            let _ = writeln!(
                output,
                "[lines {start_line}-{end_line} omitted; {omitted_bytes} bytes omitted]"
            );
        }
    } else if start_line == end_line {
        let _ = writeln!(
            output,
            "[line {start_line} contains omitted content; {omitted_bytes} bytes omitted]"
        );
    } else {
        let _ = writeln!(
            output,
            "[lines {start_line}-{end_line} contain omitted content; {omitted_bytes} bytes omitted]"
        );
    }

    let _ = writeln!(output, "Full output: {}", path.display());
    output.push_str(
        "Use read_file, grep, sed, head, or tail to inspect bounded portions.\n\
         Do not print the entire file through Bash; long output will be truncated again.\n",
    );
    output.push_str(&content[tail_start..]);

    Preview {
        content: output,
        path: Some(path.display().to_string()),
    }
}

fn fits(content: &str, limits: Limits) -> bool {
    line_count(content) <= limits.lines && content.chars().count() <= limits.chars
}

fn line_count(content: &str) -> usize {
    if content.is_empty() {
        0
    } else {
        content.bytes().filter(|byte| *byte == b'\n').count()
            + usize::from(!content.ends_with('\n'))
    }
}

fn split_limit(total: usize, left_need: usize, right_need: usize) -> (usize, usize) {
    if left_need == 0 {
        return (0, total.min(right_need));
    }
    if right_need == 0 {
        return (total.min(left_need), 0);
    }

    let (mut left, mut right) = (total.div_ceil(2), total / 2);
    if left_need < left {
        right = right.saturating_add(left - left_need);
        left = left_need;
    }
    if right_need < right {
        left = left.saturating_add(right - right_need).min(left_need);
        right = right_need;
    }

    (left, right)
}

fn prefix_end(content: &str, max_lines: usize, max_chars: usize) -> usize {
    if content.is_empty() || max_lines == 0 || max_chars == 0 {
        return 0;
    }

    let (mut chars, mut lines, mut boundary) = (0, 1, 0);
    for (index, character) in content.char_indices() {
        if chars == max_chars || lines > max_lines {
            return if boundary == 0 { index } else { boundary };
        }

        chars += 1;
        if character == '\n' {
            boundary = index + character.len_utf8();
            if lines == max_lines {
                return boundary;
            }
            lines += 1;
        }
    }

    content.len()
}

fn suffix_start(content: &str, max_lines: usize, max_chars: usize) -> usize {
    if content.is_empty() || max_lines == 0 || max_chars == 0 {
        return content.len();
    }

    let (mut start, mut chars, mut lines, mut boundary) = (content.len(), 0, 1, content.len());
    for (index, character) in content.char_indices().rev() {
        if chars == max_chars {
            let starts_line = start == 0 || content.as_bytes().get(start - 1) == Some(&b'\n');

            return if starts_line || boundary == content.len() {
                start
            } else {
                boundary
            };
        }
        if character == '\n' && lines == max_lines {
            return start;
        }

        (start, chars) = (index, chars + 1);
        if character == '\n' {
            boundary = index + character.len_utf8();
            lines += 1;
        }
    }

    start
}

fn line_at(content: &str, byte: usize) -> usize {
    content[..byte]
        .bytes()
        .filter(|value| *value == b'\n')
        .count()
        .saturating_add(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_reports_absolute_omitted_lines_and_bytes() {
        let content = (1..=10)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let path = Path::new("/tmp/full.log");
        let preview = preview(
            &content,
            path,
            Limits {
                lines: 4,
                chars: 100,
            },
        );
        let omitted = "line 3\nline 4\nline 5\nline 6\nline 7\nline 8\n";

        assert!(preview.content.contains(&format!(
            "[lines 3-8 omitted; {} bytes omitted]",
            omitted.len()
        )));
        assert!(preview.content.starts_with("line 1\nline 2\n"));
        assert!(preview.content.ends_with("line 9\nline 10"));
        assert_eq!(preview.path.as_deref(), Some("/tmp/full.log"));
    }

    #[test]
    fn character_limit_preserves_utf8_boundaries() {
        let content = "界".repeat(20);
        let preview = preview(
            &content,
            Path::new("/tmp/unicode.log"),
            Limits {
                lines: 10,
                chars: 6,
            },
        );

        assert!(preview.content.starts_with("界界界\n"));
        assert!(preview.content.ends_with("界界界"));
        assert!(preview.content.contains("line 1 contains omitted content"));
    }

    #[test]
    fn character_limit_prefers_complete_line_boundaries() {
        let content = (1..=100)
            .map(|line| format!("line {line}: {}", "x".repeat(40)))
            .collect::<Vec<_>>()
            .join("\n");
        let preview = preview(
            &content,
            Path::new("/tmp/lines.log"),
            Limits {
                lines: 500,
                chars: 200,
            },
        );

        assert!(preview.content.contains("lines 3-98 omitted"));
        assert!(!preview.content.contains("contain omitted content"));
    }

    #[test]
    fn paired_outputs_share_the_global_budget() {
        let left = "a".repeat(MAX_OUTPUT_CHARS);
        let right = "b".repeat(MAX_OUTPUT_CHARS);
        let (left_limits, right_limits) = Limits::split(&left, &right);

        assert_eq!(left_limits.chars + right_limits.chars, MAX_OUTPUT_CHARS);
        assert_eq!(left_limits.lines + right_limits.lines, 2);
    }
}
