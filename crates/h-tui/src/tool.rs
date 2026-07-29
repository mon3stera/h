use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};

use h_core::tool::{DiffLine, DiffLineKind, DisplayBlock, Presentation, ToolCallStatus};

use crate::text::wrap;

/// Diff row washes, dark enough that the default foreground stays legible on top
/// of them. A saturated fill reads as a block of colour rather than as code.
const REMOVED_WASH: Color = Color::Rgb(0x4a, 0x1e, 0x24);
const ADDED_WASH: Color = Color::Rgb(0x1c, 0x3d, 0x28);

const MUTED: Color = Color::DarkGray;

/// Where a wrapped title's later rows begin.
const TITLE_INDENT: &str = "  ";

/// The indent under a tool's title, matching the glyphs the blocks use.
const BLOCK_INDENT: &str = "   ";
const NESTED_INDENT: &str = "     ";

/// Lays out one tool call: a title, then whatever the presenter attached.
pub fn render(presentation: &Presentation, width: usize) -> Vec<Line<'static>> {
    let mut lines = title(presentation, width);

    for block in &presentation.blocks {
        lines.extend(render_block(block, width));
    }

    lines
}

/// The title, wrapped rather than left to run off the edge — a long command or
/// search pattern easily outruns a terminal.
fn title(presentation: &Presentation, width: usize) -> Vec<Line<'static>> {
    let indicator = match &presentation.status {
        ToolCallStatus::Running => "⟳ ",
        ToolCallStatus::Succeeded => "● ",
        ToolCallStatus::Failed { .. } => "✗ ",
    };

    let target = presentation.target.as_deref().unwrap_or("unknown");
    let text = format!(
        "{} {}({}) <- ({})",
        indicator, presentation.name, target, presentation.label
    );

    let inner = width.saturating_sub(TITLE_INDENT.len()).max(1);

    wrap(&crate::rainbow_spans(&text, Style::default()), inner)
        .into_iter()
        .enumerate()
        .map(|(offset, line)| {
            // A continuation is indented so it cannot be read as another entry
            // starting at the margin.
            if offset == 0 {
                return line;
            }

            let mut spans = vec![Span::raw(TITLE_INDENT)];
            spans.extend(line.spans);

            Line::from(spans)
        })
        .collect()
}

fn render_block(block: &DisplayBlock, width: usize) -> Vec<Line<'static>> {
    match block {
        DisplayBlock::Summary(summary) => {
            indented(BLOCK_INDENT, "└ ", summary, width, Style::default())
        }
        DisplayBlock::TextOutput { content, .. } => {
            indented(BLOCK_INDENT, "└ ", content, width, Style::default())
        }
        DisplayBlock::KeyValue { entries } => entries
            .iter()
            .flat_map(|entry| {
                indented(
                    NESTED_INDENT,
                    "- ",
                    &format!("{}: {}", entry.key, entry.value),
                    width,
                    Style::default(),
                )
            })
            .collect(),
        DisplayBlock::CodeBlock {
            content,
            truncated_lines,
            show_line_numbers,
            start_line_number,
            ..
        } => code_lines(
            content,
            *truncated_lines,
            *show_line_numbers,
            *start_line_number,
        ),
        DisplayBlock::Diff { lines } => diff_lines(lines),
        DisplayBlock::Table { headers, rows } => table_lines(headers, rows),
    }
}

/// Wraps `body` under a glyph, keeping continuation lines under the text.
fn indented(
    indent: &str,
    glyph: &str,
    body: &str,
    width: usize,
    style: Style,
) -> Vec<Line<'static>> {
    let lead = indent.len() + glyph.chars().count();
    let inner = width.saturating_sub(lead).max(1);

    wrap(&[Span::styled(body.to_owned(), style)], inner)
        .into_iter()
        .enumerate()
        .map(|(offset, line)| {
            let prefix = if offset == 0 {
                format!("{indent}{glyph}")
            } else {
                " ".repeat(lead)
            };

            let mut spans = vec![Span::raw(prefix)];
            spans.extend(line.spans);

            Line::from(spans)
        })
        .collect()
}

fn code_lines(
    content: &str,
    truncated_lines: usize,
    show_line_numbers: bool,
    start_line_number: usize,
) -> Vec<Line<'static>> {
    let lines = if content.is_empty() {
        vec![""]
    } else {
        content.lines().take(truncated_lines).collect::<Vec<_>>()
    };
    let last = start_line_number.saturating_add(lines.len().saturating_sub(1));
    let number_width = last.to_string().len();

    lines
        .into_iter()
        .enumerate()
        .map(|(offset, line)| {
            if show_line_numbers {
                let number = start_line_number.saturating_add(offset);

                Line::from(format!("{NESTED_INDENT}{number:>number_width$} {line}"))
            } else {
                Line::from(format!("{NESTED_INDENT}{line}"))
            }
        })
        .collect()
}

/// Lays out a diff line as `<number> <sign><text>`, washing changed rows the way
/// command-line tools colour them: removals red, additions green.
///
/// The colour is a background rather than a foreground because the row is filled
/// edge to edge; tinting the glyphs the same hue would leave them unreadable
/// against it.
fn diff_lines(lines: &[DiffLine]) -> Vec<Line<'static>> {
    let width = lines
        .iter()
        .map(|line| line.number)
        .max()
        .unwrap_or(0)
        .to_string()
        .len();

    lines
        .iter()
        .map(|line| {
            let (sign, wash) = match line.kind {
                DiffLineKind::Removed => ('-', Some(REMOVED_WASH)),
                DiffLineKind::Added => ('+', Some(ADDED_WASH)),
                DiffLineKind::Context => (' ', None),
            };

            let style = wash.map_or_else(Style::default, |wash| Style::default().bg(wash));

            Line::from(Span::styled(
                format!("{NESTED_INDENT}{:>width$} {sign}{}", line.number, line.text),
                style,
            ))
            // Filling the row to the edge is what makes the wash read as a band
            // rather than as highlighted text.
            .style(style)
        })
        .collect()
}

fn table_lines(headers: &[String], rows: &[Vec<String>]) -> Vec<Line<'static>> {
    let columns = headers
        .len()
        .max(rows.iter().map(Vec::len).max().unwrap_or(0));

    let widths = (0..columns)
        .map(|index| {
            let header = headers.get(index).map_or(0, String::len);

            rows.iter()
                .filter_map(|row| row.get(index))
                .map(String::len)
                .chain([header])
                .max()
                .unwrap_or(0)
        })
        .collect::<Vec<_>>();

    let row_line = |cells: &[String], style: Style| {
        let rendered = (0..columns)
            .map(|index| {
                let cell = cells.get(index).map(String::as_str).unwrap_or_default();

                format!("{cell:<width$}", width = widths[index])
            })
            .collect::<Vec<_>>()
            .join("  ");

        Line::from(Span::styled(format!("{NESTED_INDENT}{rendered}"), style))
    };

    let mut lines = Vec::new();

    if !headers.is_empty() {
        lines.push(row_line(headers, Style::default().fg(MUTED)));
    }

    lines.extend(rows.iter().map(|row| row_line(row, Style::default())));
    lines
}

#[cfg(test)]
mod tests {
    use h_core::tool::{KeyValueEntry, ToolCallId};

    use super::*;

    fn presentation(blocks: Vec<DisplayBlock>) -> Presentation {
        Presentation {
            call_id: ToolCallId("call-1".to_owned()),
            name: "Edit".to_owned(),
            label: "built-in".to_owned(),
            target: Some("src/main.rs".to_owned()),
            status: ToolCallStatus::Succeeded,
            blocks,
        }
    }

    fn texts(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    /// The lines a presentation's blocks draw, skipping however many the title
    /// took. Slicing a fixed offset would break the moment a title wraps.
    fn block_lines(presentation: &Presentation, width: usize) -> Vec<Line<'static>> {
        let mut bare = presentation.clone();
        bare.blocks = Vec::new();

        let title_rows = render(&bare, width).len();
        let mut lines = render(presentation, width);

        lines.split_off(title_rows)
    }

    fn block_rows(presentation: &Presentation, width: usize) -> Vec<String> {
        texts(&block_lines(presentation, width))
    }

    fn diff_line(number: usize, kind: DiffLineKind, text: &str) -> DiffLine {
        DiffLine {
            number,
            kind,
            text: text.to_owned(),
        }
    }

    #[test]
    fn the_title_reports_the_tool_and_its_status() {
        let lines = render(&presentation(Vec::new()), 60);

        assert_eq!(texts(&lines), ["●  Edit(src/main.rs) <- (built-in)"]);
    }

    #[test]
    fn a_long_title_wraps_instead_of_running_off_the_edge() {
        let mut shown = presentation(Vec::new());
        shown.name = "Bash".to_owned();
        shown.target = Some("cargo test --workspace --all-features -- --nocapture".to_owned());

        let rows = texts(&render(&shown, 30));

        assert!(rows.len() > 1, "it should have wrapped: {rows:?}");
        assert!(
            rows.iter().all(|row| row.chars().count() <= 30),
            "no row may outrun the terminal: {rows:?}"
        );
        assert!(
            rows[1].starts_with("  "),
            "a continuation is indented so it is not read as a new entry: {rows:?}"
        );
    }

    #[test]
    fn a_title_that_fits_stays_on_one_row() {
        assert_eq!(render(&presentation(Vec::new()), 60).len(), 1);
    }

    #[test]
    fn a_wrapped_title_still_carries_its_blocks() {
        let mut shown = presentation(vec![DisplayBlock::Summary("done".to_owned())]);
        shown.target = Some("a target long enough to force the title onto two rows".to_owned());

        assert_eq!(block_rows(&shown, 30), ["   └ done"]);
    }

    #[test]
    fn each_status_has_its_own_indicator() {
        let indicator = |status| {
            let mut shown = presentation(Vec::new());
            shown.status = status;

            texts(&render(&shown, 60))[0].chars().next().unwrap()
        };

        assert_eq!(indicator(ToolCallStatus::Running), '⟳');
        assert_eq!(indicator(ToolCallStatus::Succeeded), '●');
        assert_eq!(
            indicator(ToolCallStatus::Failed {
                message: "no".to_owned()
            }),
            '✗'
        );
    }

    #[test]
    fn a_summary_hangs_under_the_glyph() {
        let rows = block_rows(
            &presentation(vec![DisplayBlock::Summary(
                "alpha beta gamma delta".to_owned(),
            )]),
            20,
        );

        assert_eq!(
            rows,
            ["   └ alpha beta", "     gamma delta"],
            "the wrapped remainder lines up past the glyph"
        );
    }

    #[test]
    fn key_values_are_listed_one_per_row() {
        let rows = block_rows(
            &presentation(vec![DisplayBlock::KeyValue {
                entries: vec![KeyValueEntry {
                    key: "exit_code".to_owned(),
                    value: "0".to_owned(),
                }],
            }]),
            40,
        );

        assert_eq!(rows, ["     - exit_code: 0"]);
    }

    #[test]
    fn a_diff_puts_the_number_before_the_sign() {
        let rows = block_rows(
            &presentation(vec![DisplayBlock::Diff {
                lines: vec![
                    diff_line(9, DiffLineKind::Context, "kept"),
                    diff_line(10, DiffLineKind::Removed, "old"),
                    diff_line(10, DiffLineKind::Added, "new"),
                ],
            }]),
            40,
        );

        assert_eq!(rows, ["      9  kept", "     10 -old", "     10 +new"]);
    }

    #[test]
    fn only_changed_diff_rows_are_washed() {
        let lines = block_lines(
            &presentation(vec![DisplayBlock::Diff {
                lines: vec![
                    diff_line(1, DiffLineKind::Context, "kept"),
                    diff_line(2, DiffLineKind::Removed, "old"),
                    diff_line(2, DiffLineKind::Added, "new"),
                ],
            }]),
            40,
        );

        let backgrounds = lines.iter().map(|line| line.style.bg).collect::<Vec<_>>();

        assert_eq!(
            backgrounds,
            [None, Some(REMOVED_WASH), Some(ADDED_WASH)],
            "the wash belongs to the whole row, not to its glyphs"
        );
    }

    #[test]
    fn code_blocks_number_their_lines_when_asked() {
        let rows = block_rows(
            &presentation(vec![DisplayBlock::CodeBlock {
                language: None,
                content: "one\ntwo".to_owned(),
                truncated_lines: 10,
                show_line_numbers: true,
                start_line_number: 9,
            }]),
            40,
        );

        assert_eq!(rows, ["      9 one", "     10 two"]);
    }

    #[test]
    fn an_empty_code_block_still_gets_a_numbered_row() {
        let rows = block_rows(
            &presentation(vec![DisplayBlock::CodeBlock {
                language: None,
                content: String::new(),
                truncated_lines: 10,
                show_line_numbers: true,
                start_line_number: 42,
            }]),
            40,
        );

        assert_eq!(rows, ["     42 "]);
    }

    #[test]
    fn a_code_block_stops_at_its_truncation_limit() {
        let rows = block_rows(
            &presentation(vec![DisplayBlock::CodeBlock {
                language: None,
                content: "a\nb\nc\nd".to_owned(),
                truncated_lines: 2,
                show_line_numbers: false,
                start_line_number: 1,
            }]),
            40,
        );

        assert_eq!(rows, ["     a", "     b"]);
    }

    #[test]
    fn a_table_pads_its_columns_to_the_widest_cell() {
        let rows = block_rows(
            &presentation(vec![DisplayBlock::Table {
                headers: vec!["name".to_owned(), "n".to_owned()],
                rows: vec![vec!["a-long-name".to_owned(), "1".to_owned()]],
            }]),
            60,
        );

        assert_eq!(rows, ["     name         n", "     a-long-name  1"]);
    }
}
