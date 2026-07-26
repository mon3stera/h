use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    tui::text::{verbatim, wrap},
    ui::markdown::{Inline, MarkdownBlock, TableAlignment},
};

/// Columns taken by a bordered box: a border and a pad on each side.
const BOX_FRAME: usize = 4;
/// Columns taken by a quote's left rule and the space after it.
const QUOTE_FRAME: usize = 2;

const MUTED: Color = Color::DarkGray;
const CODE: Color = Color::Green;

/// Lays out parsed markdown as terminal lines of a given width.
///
/// Blocks are separated by a blank line, the way the old flex layout spaced them
/// with a row gap.
pub fn render(blocks: &[MarkdownBlock], width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    for (index, block) in blocks.iter().enumerate() {
        if index > 0 {
            lines.push(Line::default());
        }

        lines.extend(render_block(block, width));
    }

    lines
}

fn render_block(block: &MarkdownBlock, width: usize) -> Vec<Line<'static>> {
    match block {
        MarkdownBlock::Paragraph(content) => wrap(&spans(content, Style::default()), width),
        MarkdownBlock::Heading { level, content } => render_heading(*level, content, width),
        MarkdownBlock::CodeBlock { language, code } => render_code_block(language, code, width),
        MarkdownBlock::Quote(blocks) => render_quote(blocks, width),
        MarkdownBlock::List { start, items } => render_list(*start, items, width),
        MarkdownBlock::Table {
            alignments,
            headers,
            rows,
        } => render_table(alignments, headers, rows, width),
        MarkdownBlock::Rule => vec![rule(width)],
    }
}

fn rule(width: usize) -> Line<'static> {
    Line::from(Span::styled(
        "─".repeat(width.max(1)),
        Style::default().fg(MUTED),
    ))
}

fn render_heading(level: u8, content: &[Inline], width: usize) -> Vec<Line<'static>> {
    let style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let mut heading = vec![Span::styled(
        format!("{} ", "#".repeat(level.clamp(1, 6).into())),
        style,
    )];

    heading.extend(spans(content, style));
    wrap(&heading, width)
}

fn render_code_block(
    language: &Option<String>,
    code: &[Inline],
    width: usize,
) -> Vec<Line<'static>> {
    let inner = width.saturating_sub(BOX_FRAME).max(1);
    let border = Style::default().fg(MUTED);

    let mut body = Vec::new();

    if let Some(language) = language
        .as_deref()
        .filter(|language| !language.is_empty() && *language != "default")
    {
        body.push(Line::from(Span::styled(
            language.to_owned(),
            border.add_modifier(Modifier::BOLD),
        )));
    }

    // Indentation is part of the code, so this must not reflow.
    body.extend(verbatim(&[Span::raw(plain_text(code))], inner));

    framed(body, inner, border)
}

/// Draws a rounded box around already-wrapped lines.
fn framed(body: Vec<Line<'static>>, inner: usize, border: Style) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        format!("╭{}╮", "─".repeat(inner + 2)),
        border,
    ))];

    for line in body {
        let used = line_width(&line);
        let mut spans = vec![Span::styled("│ ", border)];

        spans.extend(line.spans);
        spans.push(Span::styled(
            format!("{} │", " ".repeat(inner.saturating_sub(used))),
            border,
        ));

        lines.push(Line::from(spans));
    }

    lines.push(Line::from(Span::styled(
        format!("╰{}╯", "─".repeat(inner + 2)),
        border,
    )));

    lines
}

fn render_quote(blocks: &[MarkdownBlock], width: usize) -> Vec<Line<'static>> {
    let inner = width.saturating_sub(QUOTE_FRAME).max(1);
    let border = Style::default().fg(MUTED);

    render(blocks, inner)
        .into_iter()
        .map(|line| {
            let mut spans = vec![Span::styled("│ ", border)];
            spans.extend(line.spans);

            Line::from(spans)
        })
        .collect()
}

fn render_list(
    start: Option<u64>,
    items: &[Vec<MarkdownBlock>],
    width: usize,
) -> Vec<Line<'static>> {
    let last_number = start.map(|start| start.saturating_add(items.len().saturating_sub(1) as u64));
    let marker_width = last_number.map_or(2, |number| decimal_digits(number) + 2);
    let inner = width.saturating_sub(marker_width).max(1);

    let mut lines = Vec::new();

    for (index, blocks) in items.iter().enumerate() {
        let marker = start.map_or_else(
            || "• ".to_owned(),
            |start| format!("{}. ", start.saturating_add(index as u64)),
        );

        for (offset, line) in render(blocks, inner).into_iter().enumerate() {
            // The marker sits on the item's first line; the rest lines up under
            // the text rather than under the marker.
            let lead = if offset == 0 {
                format!("{marker:<marker_width$}")
            } else {
                " ".repeat(marker_width)
            };

            let mut spans = vec![Span::raw(lead)];
            spans.extend(line.spans);

            lines.push(Line::from(spans));
        }
    }

    lines
}

fn render_table(
    alignments: &[TableAlignment],
    headers: &[Vec<Inline>],
    rows: &[Vec<Vec<Inline>>],
    width: usize,
) -> Vec<Line<'static>> {
    let columns = alignments
        .len()
        .max(headers.len())
        .max(rows.iter().map(Vec::len).max().unwrap_or(0));

    if columns == 0 {
        return Vec::new();
    }

    // Columns share the width evenly, as the old flex basis did. Each also pays
    // for its separator and two pads.
    let separators = columns + 1;
    let cell = width
        .saturating_sub(separators + columns * 2)
        .checked_div(columns)
        .unwrap_or(0)
        .max(1);

    let border = Style::default().fg(MUTED);
    let mut lines = vec![edge(columns, cell, border, "╭", "┬", "╮")];

    if !headers.is_empty() {
        lines.extend(table_row(
            headers,
            alignments,
            columns,
            cell,
            Style::default().add_modifier(Modifier::BOLD),
            border,
        ));
        lines.push(edge(columns, cell, border, "├", "┼", "┤"));
    }

    for row in rows {
        lines.extend(table_row(
            row,
            alignments,
            columns,
            cell,
            Style::default(),
            border,
        ));
    }

    lines.push(edge(columns, cell, border, "╰", "┴", "╯"));
    lines
}

fn edge(
    columns: usize,
    cell: usize,
    border: Style,
    left: &str,
    middle: &str,
    right: &str,
) -> Line<'static> {
    let segment = "─".repeat(cell + 2);
    let joined = (0..columns)
        .map(|_| segment.as_str())
        .collect::<Vec<_>>()
        .join(middle);

    Line::from(Span::styled(format!("{left}{joined}{right}"), border))
}

fn table_row(
    cells: &[Vec<Inline>],
    alignments: &[TableAlignment],
    columns: usize,
    cell: usize,
    style: Style,
    border: Style,
) -> Vec<Line<'static>> {
    let wrapped = (0..columns)
        .map(|index| {
            let content = cells
                .get(index)
                .map_or_else(Vec::new, |content| wrap(&spans(content, style), cell));
            let alignment = alignments
                .get(index)
                .copied()
                .unwrap_or(TableAlignment::None);

            (content, alignment)
        })
        .collect::<Vec<_>>();

    // A row is as tall as its tallest cell; the others are padded out.
    let height = wrapped
        .iter()
        .map(|(content, _)| content.len())
        .max()
        .unwrap_or(1)
        .max(1);

    (0..height)
        .map(|row| {
            let mut spans = Vec::new();

            for (content, alignment) in &wrapped {
                spans.push(Span::styled("│ ", border));

                let line = content.get(row).cloned().unwrap_or_default();
                let (before, after) = padding(line_width(&line), cell, *alignment);

                spans.push(Span::raw(" ".repeat(before)));
                spans.extend(line.spans);
                spans.push(Span::raw(" ".repeat(after)));
                spans.push(Span::raw(" "));
            }

            spans.push(Span::styled("│", border));
            Line::from(spans)
        })
        .collect()
}

/// The blanks on each side of a cell for the given alignment.
fn padding(used: usize, cell: usize, alignment: TableAlignment) -> (usize, usize) {
    let slack = cell.saturating_sub(used);

    match alignment {
        TableAlignment::None | TableAlignment::Left => (0, slack),
        TableAlignment::Right => (slack, 0),
        TableAlignment::Center => (slack / 2, slack - slack / 2),
    }
}

fn line_width(line: &Line<'static>) -> usize {
    line.spans.iter().map(|span| span.content.width()).sum()
}

/// Flattens inline markup into styled fragments, accumulating nested styles.
fn spans(content: &[Inline], style: Style) -> Vec<Span<'static>> {
    let mut result = Vec::new();
    append_spans(content, style, &mut result);
    result
}

fn append_spans(content: &[Inline], style: Style, result: &mut Vec<Span<'static>>) {
    for inline in content {
        match inline {
            Inline::Text(text) => result.push(Span::styled(text.clone(), style)),
            // A colour rather than an inverted block: a reversed run reads as a
            // hole punched in the prose.
            Inline::Code(code) => result.push(Span::styled(code.clone(), style.fg(CODE))),
            Inline::Emphasis(content) => {
                append_spans(content, style.add_modifier(Modifier::ITALIC), result);
            }
            Inline::Strong(content) => {
                append_spans(content, style.add_modifier(Modifier::BOLD), result);
            }
            Inline::Strikethrough(content) => {
                append_spans(content, style.add_modifier(Modifier::CROSSED_OUT), result);
            }
            Inline::Link { dst, content } => {
                let link = style.fg(Color::Cyan).add_modifier(Modifier::UNDERLINED);
                let label = plain_text(content);

                append_spans(content, link, result);

                // A bare URL is already its own label; repeating it says nothing.
                if !dst.is_empty() && dst != &label {
                    result.push(Span::styled(format!(" <{dst}>"), link));
                }
            }
            Inline::SoftBreak => result.push(Span::styled(" ", style)),
            Inline::HardBreak => result.push(Span::styled("\n", style)),
        }
    }
}

fn plain_text(content: &[Inline]) -> String {
    let mut result = String::new();

    for inline in content {
        match inline {
            Inline::Text(text) | Inline::Code(text) => result.push_str(text),
            Inline::Emphasis(content)
            | Inline::Strong(content)
            | Inline::Strikethrough(content)
            | Inline::Link { content, .. } => result.push_str(&plain_text(content)),
            Inline::SoftBreak => result.push(' '),
            Inline::HardBreak => result.push('\n'),
        }
    }

    result
}

fn decimal_digits(value: u64) -> usize {
    value.checked_ilog10().unwrap_or(0) as usize + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::markdown::parse_markdown;

    /// The rendered text of each line, styles ignored.
    fn render_text(source: &str, width: usize) -> Vec<String> {
        render(&parse_markdown(source), width)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .map(|line| line.trim_end().to_owned())
            .collect()
    }

    #[test]
    fn a_paragraph_wraps_to_the_given_width() {
        assert_eq!(
            render_text("alpha beta gamma delta", 12),
            ["alpha beta", "gamma delta"]
        );
    }

    #[test]
    fn blocks_are_separated_by_a_blank_line() {
        assert_eq!(render_text("first\n\nsecond", 20), ["first", "", "second"]);
    }

    #[test]
    fn a_heading_keeps_its_hashes() {
        assert_eq!(render_text("## Title", 20), ["## Title"]);
    }

    #[test]
    fn a_code_block_keeps_its_indentation() {
        assert_eq!(
            render_text("```rust\nfn main() {\n    let x = 1;\n}\n```", 30),
            [
                "╭────────────────────────────╮",
                "│ rust                       │",
                "│ fn main() {                │",
                "│     let x = 1;             │",
                "│ }                          │",
                "╰────────────────────────────╯",
            ]
        );
    }

    #[test]
    fn a_code_block_is_framed_and_labelled() {
        assert_eq!(
            render_text("```rust\nlet x = 1;\n```", 20),
            [
                "╭──────────────────╮",
                "│ rust             │",
                "│ let x = 1;       │",
                "╰──────────────────╯",
            ]
        );
    }

    #[test]
    fn a_quote_carries_a_left_rule_on_every_line() {
        assert_eq!(
            render_text("> alpha beta gamma", 12),
            ["│ alpha beta", "│ gamma"]
        );
    }

    #[test]
    fn a_bullet_list_hangs_its_continuation_under_the_text() {
        assert_eq!(
            render_text("- alpha beta gamma", 12),
            ["• alpha beta", "  gamma"],
            "the wrapped remainder lines up past the marker"
        );
    }

    #[test]
    fn an_ordered_list_numbers_its_items() {
        assert_eq!(render_text("1. one\n2. two", 20), ["1. one", "2. two"]);
    }

    #[test]
    fn an_ordered_list_reserves_room_for_its_widest_number() {
        let source = (1..=10)
            .map(|index| format!("{index}. item"))
            .collect::<Vec<_>>()
            .join("\n");

        let lines = render_text(&source, 20);

        assert_eq!(
            lines.first().unwrap(),
            "1.  item",
            "padded to match \"10.\""
        );
        assert_eq!(lines.last().unwrap(), "10. item");
    }

    #[test]
    fn a_rule_fills_the_width() {
        assert_eq!(render_text("---", 8), ["────────"]);
    }

    #[test]
    fn a_table_is_drawn_as_a_grid() {
        let lines = render_text("| a | b |\n| - | - |\n| 1 | 2 |", 20);

        assert_eq!(
            lines,
            [
                "╭────────┬────────╮",
                "│ a      │ b      │",
                "├────────┼────────┤",
                "│ 1      │ 2      │",
                "╰────────┴────────╯",
            ]
        );
    }

    #[test]
    fn a_taller_cell_sets_the_height_of_its_row() {
        let lines = render_text("| a | b |\n| - | - |\n| one two three | x |", 24);

        assert!(
            lines.iter().any(|line| line.contains("one two")),
            "{lines:?}"
        );
        assert!(lines.iter().any(|line| line.contains("three")), "{lines:?}");
    }

    #[test]
    fn inline_styles_reach_the_spans() {
        let lines = render(&parse_markdown("*italic* **bold**"), 40);
        let styles = lines[0]
            .spans
            .iter()
            .map(|span| span.style.add_modifier)
            .collect::<Vec<_>>();

        assert!(styles.iter().any(|style| style.contains(Modifier::ITALIC)));
        assert!(styles.iter().any(|style| style.contains(Modifier::BOLD)));
    }

    #[test]
    fn inline_code_is_coloured_rather_than_inverted() {
        let lines = render(&parse_markdown("run `cargo` now"), 40);
        let code = lines[0]
            .spans
            .iter()
            .find(|span| span.content == "cargo")
            .expect("the code span should survive");

        assert_eq!(code.style.fg, Some(CODE));
        assert!(
            !code.style.add_modifier.contains(Modifier::REVERSED),
            "an inverted run reads as a hole punched in the prose"
        );
    }

    #[test]
    fn a_link_shows_its_target_when_it_differs_from_the_label() {
        assert_eq!(
            render_text("[docs](https://example.com)", 40),
            ["docs <https://example.com>"]
        );
    }

    #[test]
    fn a_link_whose_label_is_its_target_is_not_repeated() {
        assert_eq!(
            render_text("<https://example.com>", 40),
            ["https://example.com"]
        );
    }
}
