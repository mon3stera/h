use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    Text(String),
    Code(String),
    Emphasis(Vec<Inline>),
    Strong(Vec<Inline>),
    Link { dst: String, content: Vec<Inline> },
    Strikethrough(Vec<Inline>),
    SoftBreak,
    HardBreak,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableAlignment {
    None,
    Left,
    Center,
    Right,
}

impl From<Alignment> for TableAlignment {
    fn from(value: Alignment) -> Self {
        match value {
            Alignment::None => Self::None,
            Alignment::Left => Self::Left,
            Alignment::Center => Self::Center,
            Alignment::Right => Self::Right,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkdownBlock {
    Paragraph(Vec<Inline>),
    Heading {
        level: u8,
        content: Vec<Inline>,
    },
    CodeBlock {
        language: Option<String>,
        code: Vec<Inline>,
    },
    Quote(Vec<MarkdownBlock>),
    List {
        start: Option<u64>,
        items: Vec<Vec<MarkdownBlock>>,
    },
    Table {
        alignments: Vec<TableAlignment>,
        headers: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    Rule,
}

enum OpenNode {
    Paragraph(Vec<Inline>),
    Heading {
        level: u8,
        content: Vec<Inline>,
    },
    Strong(Vec<Inline>),
    Emphasis(Vec<Inline>),
    Strikethrough(Vec<Inline>),
    Link {
        dst: String,
        content: Vec<Inline>,
    },
    List {
        start: Option<u64>,
        items: Vec<Vec<MarkdownBlock>>,
    },
    Item(Vec<MarkdownBlock>),
    CodeBlock {
        language: Option<String>,
        code: Vec<Inline>,
    },
    Table {
        alignments: Vec<TableAlignment>,
        headers: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    TableHead {
        cells: Vec<Vec<Inline>>,
    },
    TableRow {
        cells: Vec<Vec<Inline>>,
    },
    TableCell(Vec<Inline>),
    Quote(Vec<MarkdownBlock>),
}

impl OpenNode {
    fn matches_end(&self, end: TagEnd) -> bool {
        matches!(
            (self, end),
            (Self::Paragraph(_), TagEnd::Paragraph)
                | (Self::Heading { .. }, TagEnd::Heading(_))
                | (Self::Strong(_), TagEnd::Strong)
                | (Self::Emphasis(_), TagEnd::Emphasis)
                | (Self::Strikethrough(_), TagEnd::Strikethrough)
                | (Self::Link { .. }, TagEnd::Link)
                | (Self::List { .. }, TagEnd::List(_))
                | (Self::Item(_), TagEnd::Item)
                | (Self::CodeBlock { .. }, TagEnd::CodeBlock)
                | (Self::Table { .. }, TagEnd::Table)
                | (Self::TableHead { .. }, TagEnd::TableHead)
                | (Self::TableRow { .. }, TagEnd::TableRow)
                | (Self::TableCell(_), TagEnd::TableCell)
                | (Self::Quote(_), TagEnd::BlockQuote(_))
        )
    }
}

fn supports_tag(tag: &Tag<'_>) -> bool {
    matches!(
        tag,
        Tag::Paragraph
            | Tag::Heading { .. }
            | Tag::CodeBlock(_)
            | Tag::BlockQuote(_)
            | Tag::List(_)
            | Tag::Item
            | Tag::Emphasis
            | Tag::Strong
            | Tag::Strikethrough
            | Tag::Link { .. }
            | Tag::Table(_)
            | Tag::TableHead
            | Tag::TableRow
            | Tag::TableCell
    )
}

struct UnsupportedNode {
    depth: usize,
}

struct State {
    parsed: Vec<MarkdownBlock>,
    stack: Vec<OpenNode>,
    unsupported: Option<UnsupportedNode>,
}

impl State {
    fn new() -> Self {
        Self {
            parsed: Vec::new(),
            stack: Vec::new(),
            unsupported: None,
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        let node = match tag {
            Tag::Paragraph => OpenNode::Paragraph(Vec::new()),
            Tag::Heading { level, .. } => OpenNode::Heading {
                level: level as u8,
                content: Vec::new(),
            },
            Tag::CodeBlock(kind) => {
                let language = match kind {
                    CodeBlockKind::Indented => Some("default".to_owned()),
                    CodeBlockKind::Fenced(language) => Some(language.into_string()),
                };

                OpenNode::CodeBlock {
                    language,
                    code: Vec::new(),
                }
            }
            Tag::Strong => OpenNode::Strong(Vec::new()),
            Tag::Emphasis => OpenNode::Emphasis(Vec::new()),
            Tag::Link { dest_url, .. } => OpenNode::Link {
                dst: dest_url.into_string(),
                content: Vec::new(),
            },
            Tag::Strikethrough => OpenNode::Strikethrough(Vec::new()),
            Tag::BlockQuote(_) => OpenNode::Quote(Vec::new()),
            Tag::List(start) => OpenNode::List {
                start,
                items: Vec::new(),
            },
            Tag::Item => OpenNode::Item(Vec::new()),
            Tag::Table(alignments) => OpenNode::Table {
                alignments: alignments.into_iter().map(Into::into).collect(),
                headers: Vec::new(),
                rows: Vec::new(),
            },
            Tag::TableHead => OpenNode::TableHead { cells: Vec::new() },
            Tag::TableRow => OpenNode::TableRow { cells: Vec::new() },
            Tag::TableCell => OpenNode::TableCell(Vec::new()),
            _ => return,
        };

        self.stack.push(node);
    }

    fn end(&mut self, end: TagEnd) {
        let Some(index) = self.stack.iter().rposition(|node| node.matches_end(end)) else {
            return;
        };

        while self.stack.len() > index {
            let Some(node) = self.stack.pop() else {
                break;
            };
            self.merge_node(node);
        }
    }

    fn merge_node(&mut self, node: OpenNode) {
        match node {
            OpenNode::Paragraph(content) => self.append_block(MarkdownBlock::Paragraph(content)),
            OpenNode::Heading { level, content } => {
                self.append_block(MarkdownBlock::Heading { level, content })
            }
            OpenNode::Strong(content) => self.append_inline(Inline::Strong(content)),
            OpenNode::Emphasis(content) => self.append_inline(Inline::Emphasis(content)),
            OpenNode::Strikethrough(content) => self.append_inline(Inline::Strikethrough(content)),
            OpenNode::Link { dst, content } => self.append_inline(Inline::Link { dst, content }),
            OpenNode::List { start, items } => {
                self.append_block(MarkdownBlock::List { start, items })
            }
            OpenNode::Item(blocks) => match self.stack.last_mut() {
                Some(OpenNode::List { items, .. }) => items.push(blocks),
                _ => {
                    for block in blocks {
                        self.append_block(block);
                    }
                }
            },
            OpenNode::CodeBlock { language, code } => {
                self.append_block(MarkdownBlock::CodeBlock { language, code })
            }
            OpenNode::Table {
                alignments,
                headers,
                rows,
            } => self.append_block(MarkdownBlock::Table {
                alignments,
                headers,
                rows,
            }),
            OpenNode::TableHead { cells } => match self.stack.last_mut() {
                Some(OpenNode::Table { headers, .. }) => *headers = cells,
                _ => self.append_cells_as_paragraphs(cells),
            },
            OpenNode::TableRow { cells } => match self.stack.last_mut() {
                Some(OpenNode::Table { rows, .. }) => rows.push(cells),
                _ => self.append_cells_as_paragraphs(cells),
            },
            OpenNode::TableCell(content) => match self.stack.last_mut() {
                Some(OpenNode::TableHead { cells }) | Some(OpenNode::TableRow { cells }) => {
                    cells.push(content)
                }
                _ => self.append_block(MarkdownBlock::Paragraph(content)),
            },
            OpenNode::Quote(blocks) => self.append_block(MarkdownBlock::Quote(blocks)),
        }
    }

    fn append_cells_as_paragraphs(&mut self, cells: Vec<Vec<Inline>>) {
        for cell in cells {
            self.append_block(MarkdownBlock::Paragraph(cell));
        }
    }

    fn append_inline(&mut self, inline: Inline) {
        match self.stack.last_mut() {
            Some(OpenNode::Paragraph(content))
            | Some(OpenNode::Strong(content))
            | Some(OpenNode::Emphasis(content))
            | Some(OpenNode::Strikethrough(content))
            | Some(OpenNode::TableCell(content))
            | Some(OpenNode::CodeBlock { code: content, .. }) => content.push(inline),
            Some(OpenNode::Heading { content, .. }) | Some(OpenNode::Link { content, .. }) => {
                content.push(inline)
            }
            Some(OpenNode::Item(blocks)) | Some(OpenNode::Quote(blocks)) => {
                Self::append_inline_to_blocks(blocks, inline)
            }
            Some(OpenNode::List { items, .. }) => {
                if let Some(item) = items.last_mut() {
                    Self::append_inline_to_blocks(item, inline);
                } else {
                    items.push(vec![MarkdownBlock::Paragraph(vec![inline])]);
                }
            }
            Some(OpenNode::TableHead { cells }) | Some(OpenNode::TableRow { cells }) => {
                cells.push(vec![inline])
            }
            Some(OpenNode::Table { headers, .. }) => headers.push(vec![inline]),
            None => self.parsed.push(MarkdownBlock::Paragraph(vec![inline])),
        }
    }

    fn append_inline_to_blocks(blocks: &mut Vec<MarkdownBlock>, inline: Inline) {
        match blocks.last_mut() {
            Some(MarkdownBlock::Paragraph(content)) => content.push(inline),
            _ => blocks.push(MarkdownBlock::Paragraph(vec![inline])),
        }
    }

    fn append_block(&mut self, block: MarkdownBlock) {
        if let Some(parent) = self.stack.iter_mut().rev().find(|node| {
            matches!(
                node,
                OpenNode::Item(_) | OpenNode::Quote(_) | OpenNode::List { .. }
            )
        }) {
            match parent {
                OpenNode::Item(blocks) | OpenNode::Quote(blocks) => blocks.push(block),
                OpenNode::List { items, .. } => items.push(vec![block]),
                _ => {}
            }
            return;
        }

        self.parsed.push(block);
    }

    fn text(&mut self, text: impl Into<String>) {
        self.append_inline(Inline::Text(text.into()));
    }

    fn code(&mut self, code: impl Into<String>) {
        self.append_inline(Inline::Code(code.into()));
    }

    fn softbreak(&mut self) {
        self.append_inline(Inline::SoftBreak);
    }

    fn hardbreak(&mut self) {
        self.append_inline(Inline::HardBreak);
    }

    fn rule(&mut self) {
        self.append_block(MarkdownBlock::Rule);
    }

    fn task_list_marker(&mut self, checked: bool) {
        self.append_inline(Inline::Text(if checked {
            "[x] ".to_owned()
        } else {
            "[ ] ".to_owned()
        }));
    }

    fn plain_text(&mut self, text: impl Into<String>) {
        self.append_inline(Inline::Text(text.into()));
    }

    fn finalize(mut self) -> Vec<MarkdownBlock> {
        while let Some(node) = self.stack.pop() {
            self.merge_node(node);
        }
        self.parsed
    }
}

pub fn parse_markdown(markdown: &str) -> Vec<MarkdownBlock> {
    let mut options = Options::empty();

    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_FOOTNOTES);

    let mut state = State::new();
    let parser = Parser::new_ext(markdown, options);

    parser.into_offset_iter().for_each(|(event, offset)| {
        if let Some(unsupported) = &mut state.unsupported {
            match event {
                Event::Start(_) => unsupported.depth += 1,
                Event::End(_) => {
                    unsupported.depth = unsupported.depth.saturating_sub(1);
                    if unsupported.depth == 0 {
                        state.unsupported = None;
                    }
                }
                _ => {}
            }
            return;
        }

        match event {
            Event::Start(tag) if !supports_tag(&tag) => {
                state.plain_text(&markdown[offset]);
                state.unsupported = Some(UnsupportedNode { depth: 1 });
            }
            Event::Start(tag) => state.start(tag),
            Event::Text(text) => state.text(text),
            Event::Code(code) => state.code(code),
            Event::SoftBreak => state.softbreak(),
            Event::HardBreak => state.hardbreak(),
            Event::Rule => state.rule(),
            Event::TaskListMarker(checked) => state.task_list_marker(checked),
            Event::End(end) => state.end(end),
            _ => state.plain_text(&markdown[offset]),
        }
    });

    state.finalize()
}

#[cfg(test)]
mod tests {
    use super::{Inline, MarkdownBlock, TableAlignment, parse_markdown};

    #[test]
    fn parses_plain_paragraph() {
        let parsed = parse_markdown("hello");

        let [MarkdownBlock::Paragraph(content)] = parsed.as_slice() else {
            panic!("expected one paragraph, got {parsed:#?}");
        };
        assert_text(content, "hello");
    }

    #[test]
    fn parses_multiple_top_level_blocks() {
        let parsed = parse_markdown("first\n\nsecond");

        let [
            MarkdownBlock::Paragraph(first),
            MarkdownBlock::Paragraph(second),
        ] = parsed.as_slice()
        else {
            panic!("expected two paragraphs, got {parsed:#?}");
        };

        assert_text(first, "first");
        assert_text(second, "second");
    }

    #[test]
    fn parses_nested_inline_formatting() {
        let parsed = parse_markdown("before **bold and *italic*** after");

        let [MarkdownBlock::Paragraph(content)] = parsed.as_slice() else {
            panic!("expected one paragraph, got {parsed:#?}");
        };
        let [
            Inline::Text(before),
            Inline::Strong(strong),
            Inline::Text(after),
        ] = content.as_slice()
        else {
            panic!("expected text, strong, text; got {content:#?}");
        };
        let [Inline::Text(bold), Inline::Emphasis(emphasis)] = strong.as_slice() else {
            panic!("expected text and emphasis inside strong, got {strong:#?}");
        };

        assert_eq!(before, "before ");
        assert_eq!(bold, "bold and ");
        assert_text(emphasis, "italic");
        assert_eq!(after, " after");
    }

    #[test]
    fn parses_heading_with_inline_code() {
        let parsed = parse_markdown("# Use `cargo test`");

        let [MarkdownBlock::Heading { level, content }] = parsed.as_slice() else {
            panic!("expected one heading, got {parsed:#?}");
        };
        let [Inline::Text(prefix), Inline::Code(code)] = content.as_slice() else {
            panic!("expected heading text and inline code, got {content:#?}");
        };

        assert_eq!(*level, 1);
        assert_eq!(prefix, "Use ");
        assert_eq!(code, "cargo test");
    }

    #[test]
    fn parses_quote_as_nested_blocks() {
        let parsed = parse_markdown("> quoted");

        let [MarkdownBlock::Quote(blocks)] = parsed.as_slice() else {
            panic!("expected one quote, got {parsed:#?}");
        };
        let [MarkdownBlock::Paragraph(content)] = blocks.as_slice() else {
            panic!("expected a paragraph inside the quote, got {blocks:#?}");
        };

        assert_text(content, "quoted");
    }

    #[test]
    fn parses_lists_and_preserves_ordered_start() {
        let parsed = parse_markdown("- first\n- second\n\n3. third\n4. fourth");

        let [
            MarkdownBlock::List { start: None, items },
            MarkdownBlock::List {
                start: Some(3),
                items: ordered_items,
            },
        ] = parsed.as_slice()
        else {
            panic!("expected unordered and ordered lists, got {parsed:#?}");
        };

        assert_eq!(items.len(), 2);
        assert_list_item_text(&items[0], "first");
        assert_list_item_text(&items[1], "second");
        assert_eq!(ordered_items.len(), 2);
        assert_list_item_text(&ordered_items[0], "third");
        assert_list_item_text(&ordered_items[1], "fourth");
    }

    #[test]
    fn preserves_task_list_markers_with_spacing() {
        let parsed = parse_markdown("- [ ] todo\n- [x] done");

        let [MarkdownBlock::List { items, .. }] = parsed.as_slice() else {
            panic!("expected one task list, got {parsed:#?}");
        };

        let [MarkdownBlock::Paragraph(todo)] = items[0].as_slice() else {
            panic!("expected first task item paragraph, got {:?}", items[0]);
        };
        let [MarkdownBlock::Paragraph(done)] = items[1].as_slice() else {
            panic!("expected second task item paragraph, got {:?}", items[1]);
        };

        assert_eq!(inline_text(todo), "[ ] todo");
        assert_eq!(inline_text(done), "[x] done");
    }

    #[test]
    fn parses_fenced_code_block_without_inline_markdown() {
        let parsed = parse_markdown("```rust\nfn main() {}\n```");

        let [MarkdownBlock::CodeBlock { language, code }] = parsed.as_slice() else {
            panic!("expected one code block, got {parsed:#?}");
        };

        assert_eq!(language.as_deref(), Some("rust"));
        assert_eq!(inline_text(code), "fn main() {}\n");
    }

    #[test]
    fn distinguishes_soft_and_hard_breaks() {
        let parsed = parse_markdown("first  \nsecond\nthird");

        let [MarkdownBlock::Paragraph(content)] = parsed.as_slice() else {
            panic!("expected one paragraph, got {parsed:#?}");
        };
        let [
            Inline::Text(first),
            Inline::HardBreak,
            Inline::Text(second),
            Inline::SoftBreak,
            Inline::Text(third),
        ] = content.as_slice()
        else {
            panic!("expected text separated by hard and soft breaks, got {content:#?}");
        };

        assert_eq!(first, "first");
        assert_eq!(second, "second");
        assert_eq!(third, "third");
    }

    #[test]
    fn parses_strikethrough_with_nested_inline_content() {
        let parsed = parse_markdown("before ~~deleted and **important**~~ after");

        let [MarkdownBlock::Paragraph(content)] = parsed.as_slice() else {
            panic!("expected one paragraph, got {parsed:#?}");
        };
        let [
            Inline::Text(before),
            Inline::Strikethrough(strikethrough),
            Inline::Text(after),
        ] = content.as_slice()
        else {
            panic!("expected text, strikethrough, text; got {content:#?}");
        };
        let [Inline::Text(deleted), Inline::Strong(strong)] = strikethrough.as_slice() else {
            panic!("expected text and strong inside strikethrough, got {strikethrough:#?}");
        };

        assert_eq!(before, "before ");
        assert_eq!(deleted, "deleted and ");
        assert_text(strong, "important");
        assert_eq!(after, " after");
    }

    #[test]
    fn parses_table_cells_and_alignments() {
        let parsed = parse_markdown(
            "| left | center | right |\n| :--- | :---: | ---: |\n| a | **b** | `c` |",
        );

        let [
            MarkdownBlock::Table {
                alignments,
                headers,
                rows,
            },
        ] = parsed.as_slice()
        else {
            panic!("expected one table, got {parsed:#?}");
        };

        assert_eq!(
            alignments,
            &[
                TableAlignment::Left,
                TableAlignment::Center,
                TableAlignment::Right,
            ]
        );
        assert_eq!(headers.len(), 3);
        assert_text(&headers[0], "left");
        assert_text(&headers[1], "center");
        assert_text(&headers[2], "right");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].len(), 3);
        assert_text(&rows[0][0], "a");
        assert!(matches!(&rows[0][1][..], [Inline::Strong(_)]));
        assert!(matches!(&rows[0][2][..], [Inline::Code(code)] if code == "c"));
    }

    #[test]
    fn parses_horizontal_rules_in_nested_blocks() {
        let parsed = parse_markdown("before\n\n---\n\n> quoted\n>\n> ---\n\n- item\n  \n  ---");

        assert!(matches!(parsed.get(1), Some(MarkdownBlock::Rule)));

        let quote = parsed
            .iter()
            .find_map(|block| match block {
                MarkdownBlock::Quote(blocks) => Some(blocks),
                _ => None,
            })
            .expect("expected quote");
        assert!(
            quote
                .iter()
                .any(|block| matches!(block, MarkdownBlock::Rule))
        );

        let list = parsed
            .iter()
            .find_map(|block| match block {
                MarkdownBlock::List { items, .. } => Some(items),
                _ => None,
            })
            .expect("expected list");
        assert!(
            list[0]
                .iter()
                .any(|block| matches!(block, MarkdownBlock::Rule))
        );
    }

    #[test]
    fn streaming_intermediate_markdown_does_not_panic() {
        for markdown in [
            "",
            "#",
            "**unfinished",
            "```rust\nfn main() {",
            "| a | b |\n| --- |",
        ] {
            let _ = parse_markdown(markdown);
        }
    }

    #[test]
    fn preserves_unsupported_image_as_inline_text() {
        let parsed = parse_markdown("before ![alt **bold**](image.png) after");

        let [MarkdownBlock::Paragraph(content)] = parsed.as_slice() else {
            panic!("expected one paragraph, got {parsed:#?}");
        };
        let [
            Inline::Text(before),
            Inline::Text(image),
            Inline::Text(after),
        ] = content.as_slice()
        else {
            panic!("expected image markdown to fall back to inline text, got {content:#?}");
        };

        assert_eq!(before, "before ");
        assert_eq!(image, "![alt **bold**](image.png)");
        assert_eq!(after, " after");
    }

    #[test]
    fn preserves_unsupported_inline_html_as_inline_text() {
        let parsed = parse_markdown("before <mark>highlighted</mark> after");

        let [MarkdownBlock::Paragraph(content)] = parsed.as_slice() else {
            panic!("expected one paragraph, got {parsed:#?}");
        };
        let [
            Inline::Text(before),
            Inline::Text(open_tag),
            Inline::Text(highlighted),
            Inline::Text(close_tag),
            Inline::Text(after),
        ] = content.as_slice()
        else {
            panic!("expected inline HTML to fall back to inline text, got {content:#?}");
        };

        assert_eq!(before, "before ");
        assert_eq!(open_tag, "<mark>");
        assert_eq!(highlighted, "highlighted");
        assert_eq!(close_tag, "</mark>");
        assert_eq!(after, " after");
    }

    #[test]
    fn preserves_unsupported_image_inside_tight_list_item() {
        let parsed = parse_markdown("- before ![alt](image.png) after");

        let [MarkdownBlock::List { start: None, items }] = parsed.as_slice() else {
            panic!("expected one unordered list, got {parsed:#?}");
        };
        let [item] = items.as_slice() else {
            panic!("expected one list item, got {items:#?}");
        };
        let [MarkdownBlock::Paragraph(content)] = item.as_slice() else {
            panic!("expected an implicit paragraph in the list item, got {item:#?}");
        };
        let [
            Inline::Text(before),
            Inline::Text(image),
            Inline::Text(after),
        ] = content.as_slice()
        else {
            panic!("expected image markdown to stay inline in the item, got {content:#?}");
        };

        assert_eq!(before, "before ");
        assert_eq!(image, "![alt](image.png)");
        assert_eq!(after, " after");
    }

    #[test]
    fn preserves_unsupported_top_level_block_as_text_paragraph() {
        let markdown = "<details>\ncontent\n</details>\n";
        let parsed = parse_markdown(markdown);

        let [MarkdownBlock::Paragraph(content)] = parsed.as_slice() else {
            panic!("expected unsupported block to fall back to a paragraph, got {parsed:#?}");
        };

        assert_text(content, markdown);
    }

    fn assert_text(content: &[Inline], expected: &str) {
        let [Inline::Text(actual)] = content else {
            panic!("expected one text inline, got {content:#?}");
        };

        assert_eq!(actual, expected);
    }

    fn assert_list_item_text(item: &[MarkdownBlock], expected: &str) {
        let [MarkdownBlock::Paragraph(content)] = item else {
            panic!("expected one paragraph in list item, got {item:#?}");
        };

        assert_text(content, expected);
    }

    fn inline_text(content: &[Inline]) -> String {
        content
            .iter()
            .map(|inline| match inline {
                Inline::Text(text) => text.as_str(),
                _ => "",
            })
            .collect()
    }
}
