use iocraft::prelude::*;

use super::markdown::{Inline, MarkdownBlock, TableAlignment};

#[derive(Clone, Copy, Default)]
struct InlineStyle {
    color: Option<Color>,
    weight: Weight,
    decoration: TextDecoration,
    italic: bool,
    invert: bool,
}

impl InlineStyle {
    fn content(self, text: impl ToString) -> MixedTextContent {
        let mut content = MixedTextContent::new(text).weight(self.weight);

        if let Some(color) = self.color {
            content = content.color(color);
        }
        if self.decoration != TextDecoration::None {
            content = content.decoration(self.decoration);
        }
        if self.italic {
            content = content.italic();
        }
        if self.invert {
            content = content.invert();
        }

        content
    }
}

pub(super) fn render_markdown(blocks: &[MarkdownBlock]) -> AnyElement<'static> {
    element! {
        View(width: 100pct, flex_direction: FlexDirection::Column, row_gap: 1) {
            #(blocks.iter().map(render_block))
        }
    }
    .into_any()
}

pub(super) fn render_rule() -> AnyElement<'static> {
    element! {
        View(
            width: 100pct,
            height: 1,
            border_style: BorderStyle::Single,
            border_edges: Some(Edges::Top),
        )
    }
    .into_any()
}

fn render_block(block: &MarkdownBlock) -> AnyElement<'static> {
    match block {
        MarkdownBlock::Paragraph(content) => render_inline(content, InlineStyle::default()),
        MarkdownBlock::Heading { level, content } => render_heading(*level, content),
        MarkdownBlock::CodeBlock { language, code } => render_code_block(language, code),
        MarkdownBlock::Quote(blocks) => element! {
            View(
                width: 100pct,
                flex_direction: FlexDirection::Column,
                border_style: BorderStyle::Single,
                border_edges: Some(Edges::Left),
                padding_left: 1,
            ) {
                #(blocks.iter().map(render_block))
            }
        }
        .into_any(),
        MarkdownBlock::List { start, items } => render_list(*start, items),
        MarkdownBlock::Table {
            alignments,
            headers,
            rows,
        } => render_table(alignments, headers, rows),
        MarkdownBlock::Rule => render_rule(),
    }
}

fn render_heading(level: u8, content: &[Inline]) -> AnyElement<'static> {
    let style = InlineStyle {
        color: Some(Color::Cyan),
        weight: Weight::Bold,
        ..Default::default()
    };
    let mut contents = vec![style.content(format!("{} ", "#".repeat(level.clamp(1, 6).into())))];
    append_inline_contents(content, style, &mut contents);

    element! {
        MixedText(contents: contents)
    }
    .into_any()
}

fn render_code_block(language: &Option<String>, code: &[Inline]) -> AnyElement<'static> {
    let language = language
        .as_deref()
        .filter(|language| !language.is_empty() && *language != "default")
        .map(|language| {
            element! {
                Text(
                    content: language.to_owned(),
                    color: Some(Color::DarkGrey),
                    weight: Weight::Bold,
                )
            }
            .into_any()
        });
    let code = inline_plain_text(code);

    element! {
        View(
            width: 100pct,
            flex_direction: FlexDirection::Column,
            border_style: BorderStyle::Round,
            padding_left: 1,
            padding_right: 1,
        ) {
            #(language)
            Text(content: code)
        }
    }
    .into_any()
}

fn render_list(start: Option<u64>, items: &[Vec<MarkdownBlock>]) -> AnyElement<'static> {
    let last_number = start.map(|start| start.saturating_add(items.len().saturating_sub(1) as u64));
    let marker_width = last_number.map_or(2, |number| decimal_digits(number) + 2);

    let items = items
        .iter()
        .enumerate()
        .map(|(index, blocks)| {
            let marker = start.map_or_else(
                || "• ".to_owned(),
                |start| format!("{}. ", start.saturating_add(index as u64)),
            );

            element! {
                View(width: 100pct, flex_direction: FlexDirection::Row) {
                    View(width: marker_width as u16, flex_shrink: 0.0_f32) {
                        Text(content: marker, wrap: TextWrap::NoWrap)
                    }
                    View(flex_grow: 1.0_f32, min_width: 0) {
                        #(render_markdown(blocks))
                    }
                }
            }
            .into_any()
        })
        .collect::<Vec<_>>();

    element! {
        View(width: 100pct, flex_direction: FlexDirection::Column) {
            #(items)
        }
    }
    .into_any()
}

fn render_table(
    alignments: &[TableAlignment],
    headers: &[Vec<Inline>],
    rows: &[Vec<Vec<Inline>>],
) -> AnyElement<'static> {
    let column_count = alignments
        .len()
        .max(headers.len())
        .max(rows.iter().map(Vec::len).max().unwrap_or(0));

    if column_count == 0 {
        return element! { View(width: 100pct) }.into_any();
    }

    let header = (!headers.is_empty())
        .then(|| render_table_row(headers, alignments, column_count, true, true));
    let rows = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            render_table_row(
                row,
                alignments,
                column_count,
                false,
                headers.is_empty() && index == 0,
            )
        })
        .collect::<Vec<_>>();

    element! {
        View(width: 100pct, flex_direction: FlexDirection::Column) {
            #(header)
            #(rows)
        }
    }
    .into_any()
}

fn render_table_row(
    cells: &[Vec<Inline>],
    alignments: &[TableAlignment],
    column_count: usize,
    header: bool,
    first_row: bool,
) -> AnyElement<'static> {
    let cells = (0..column_count)
        .map(|index| {
            let style = InlineStyle {
                weight: if header { Weight::Bold } else { Weight::Normal },
                ..Default::default()
            };
            let contents = cells
                .get(index)
                .map(|cell| inline_contents(cell, style))
                .unwrap_or_else(|| vec![style.content("")]);
            let align = match alignments
                .get(index)
                .copied()
                .unwrap_or(TableAlignment::None)
            {
                TableAlignment::None | TableAlignment::Left => TextAlign::Left,
                TableAlignment::Center => TextAlign::Center,
                TableAlignment::Right => TextAlign::Right,
            };

            let border_edges = (if first_row {
                Edges::Top
            } else {
                Edges::empty()
            }) | Edges::Bottom
                | Edges::Right
                | if index == 0 {
                    Edges::Left
                } else {
                    Edges::empty()
                };

            element! {
                View(
                    flex_grow: 1.0_f32,
                    flex_basis: FlexBasis::Length(0),
                    min_width: 0,
                    border_style: BorderStyle::Single,
                    border_edges: Some(border_edges),
                    padding_left: 1,
                    padding_right: 1,
                ) {
                    MixedText(contents: contents, align: align)
                }
            }
            .into_any()
        })
        .collect::<Vec<_>>();

    element! {
        View(width: 100pct, flex_direction: FlexDirection::Row) {
            #(cells)
        }
    }
    .into_any()
}

fn render_inline(content: &[Inline], style: InlineStyle) -> AnyElement<'static> {
    element! {
        MixedText(contents: inline_contents(content, style))
    }
    .into_any()
}

fn inline_contents(content: &[Inline], style: InlineStyle) -> Vec<MixedTextContent> {
    let mut result = Vec::new();
    append_inline_contents(content, style, &mut result);
    result
}

fn append_inline_contents(
    content: &[Inline],
    style: InlineStyle,
    result: &mut Vec<MixedTextContent>,
) {
    for inline in content {
        match inline {
            Inline::Text(text) => result.push(style.content(text)),
            Inline::Code(code) => result.push(
                InlineStyle {
                    invert: true,
                    ..style
                }
                .content(code),
            ),
            Inline::Emphasis(content) => append_inline_contents(
                content,
                InlineStyle {
                    italic: true,
                    ..style
                },
                result,
            ),
            Inline::Strong(content) => append_inline_contents(
                content,
                InlineStyle {
                    weight: Weight::Bold,
                    ..style
                },
                result,
            ),
            Inline::Link { dst, content } => {
                let link_style = InlineStyle {
                    color: Some(Color::Cyan),
                    decoration: TextDecoration::Underline,
                    ..style
                };
                let label = inline_plain_text(content);
                append_inline_contents(content, link_style, result);
                if !dst.is_empty() && dst != &label {
                    result.push(link_style.content(format!(" <{dst}>")));
                }
            }
            Inline::Strikethrough(content) => {
                result.push(style.content("~~"));
                append_inline_contents(content, style, result);
                result.push(style.content("~~"));
            }
            Inline::SoftBreak => result.push(style.content(" ")),
            Inline::HardBreak => result.push(style.content("\n")),
        }
    }
}

fn inline_plain_text(content: &[Inline]) -> String {
    let mut result = String::new();

    for inline in content {
        match inline {
            Inline::Text(text) | Inline::Code(text) => result.push_str(text),
            Inline::Emphasis(content)
            | Inline::Strong(content)
            | Inline::Strikethrough(content) => result.push_str(&inline_plain_text(content)),
            Inline::Link { content, .. } => result.push_str(&inline_plain_text(content)),
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

    #[test]
    fn accumulates_nested_inline_styles() {
        let contents = inline_contents(
            &[Inline::Strong(vec![Inline::Emphasis(vec![Inline::Text(
                "nested".to_owned(),
            )])])],
            InlineStyle::default(),
        );

        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0].text, "nested");
        assert_eq!(contents[0].weight, Weight::Bold);
        assert!(contents[0].italic);
    }

    #[test]
    fn renders_inline_fallbacks() {
        let contents = inline_contents(
            &[
                Inline::Code("code".to_owned()),
                Inline::SoftBreak,
                Inline::Strikethrough(vec![Inline::Text("old".to_owned())]),
                Inline::HardBreak,
                Inline::Link {
                    dst: "https://example.com".to_owned(),
                    content: vec![Inline::Text("site".to_owned())],
                },
            ],
            InlineStyle::default(),
        );
        let text = contents
            .iter()
            .map(|content| content.text.as_str())
            .collect::<String>();

        assert_eq!(text, "code ~~old~~\nsite <https://example.com>");
        assert!(contents[0].invert);
        assert!(contents.iter().any(
            |content| content.text == "site" && content.decoration == TextDecoration::Underline
        ));
    }

    #[test]
    fn renders_ordered_list_from_its_declared_start() {
        let list = MarkdownBlock::List {
            start: Some(3),
            items: vec![
                vec![MarkdownBlock::Paragraph(vec![Inline::Text(
                    "third".to_owned(),
                )])],
                vec![MarkdownBlock::Paragraph(vec![Inline::Text(
                    "fourth".to_owned(),
                )])],
            ],
        };

        let rendered = element! {
            View(width: 20) {
                #(render_block(&list))
            }
        }
        .to_string();

        assert!(
            rendered.contains("3. third"),
            "unexpected list:\n{rendered}"
        );
        assert!(
            rendered.contains("4. fourth"),
            "unexpected list:\n{rendered}"
        );
    }

    #[test]
    fn renders_all_block_types_at_fixed_width() {
        let blocks = vec![
            MarkdownBlock::Heading {
                level: 2,
                content: vec![Inline::Text("Heading".to_owned())],
            },
            MarkdownBlock::Paragraph(vec![Inline::Text("Paragraph".to_owned())]),
            MarkdownBlock::CodeBlock {
                language: Some("rust".to_owned()),
                code: vec![Inline::Text("fn main() {}".to_owned())],
            },
            MarkdownBlock::Quote(vec![MarkdownBlock::Paragraph(vec![Inline::Text(
                "Quote".to_owned(),
            )])]),
            MarkdownBlock::List {
                start: None,
                items: vec![vec![MarkdownBlock::Paragraph(vec![Inline::Text(
                    "Item".to_owned(),
                )])]],
            },
            MarkdownBlock::Table {
                alignments: vec![TableAlignment::Left, TableAlignment::Right],
                headers: vec![
                    vec![Inline::Text("A".to_owned())],
                    vec![Inline::Text("B".to_owned())],
                ],
                rows: vec![vec![vec![Inline::Text("一".to_owned())]]],
            },
            MarkdownBlock::Rule,
        ];

        let rendered = element! {
            View(width: 40) {
                #(render_markdown(&blocks))
            }
        }
        .to_string();

        for expected in [
            "## Heading",
            "Paragraph",
            "fn main() {}",
            "Quote",
            "Item",
            "一",
        ] {
            assert!(
                rendered.contains(expected),
                "missing {expected:?} in:\n{rendered}"
            );
        }
    }
}
