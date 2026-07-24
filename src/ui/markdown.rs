use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
pub enum MarkdownBlock {
    PlainText(String),
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
        ordered: bool,
        items: Vec<Vec<MarkdownBlock>>,
    },
    Table {
        headers: Vec<Inline>,
        rows: Vec<Vec<Inline>>,
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
    Emphsis(Vec<Inline>),
    Strikethrough(Vec<Inline>),
    Link {
        dst: String,
        content: Vec<Inline>,
    },
    List {
        ordered: bool,
        items: Vec<Vec<MarkdownBlock>>,
    },
    Item(Vec<MarkdownBlock>),
    CodeBlock {
        language: Option<String>,
        code: Vec<Inline>,
    },
    Table {
        headers: Vec<Inline>,
        rows: Vec<Vec<Inline>>,
    },
    TableHeader(Vec<Inline>),
    TableRow(Vec<Inline>),
    Quote(Vec<MarkdownBlock>),
}

impl OpenNode {
    fn is_inline(&self) -> bool {
        matches!(
            self,
            OpenNode::Strong(_)
                | OpenNode::Emphsis(_)
                | OpenNode::Link { .. }
                | OpenNode::Strikethrough(_)
        )
    }

    fn is_block(&self) -> bool {
        !self.is_inline()
    }
}

impl Into<MarkdownBlock> for OpenNode {
    fn into(self) -> MarkdownBlock {
        match self {
            OpenNode::Paragraph(inline) => MarkdownBlock::Paragraph(inline),
            OpenNode::Heading { level, content } => MarkdownBlock::Heading { level, content },
            OpenNode::CodeBlock { language, code } => MarkdownBlock::CodeBlock { language, code },
            OpenNode::Quote(q) => MarkdownBlock::Quote(q),
            OpenNode::List { ordered, items } => MarkdownBlock::List { ordered, items },
            OpenNode::Table { headers, rows } => MarkdownBlock::Table { headers, rows },
            _ => unreachable!(),
        }
    }
}

impl Into<Inline> for OpenNode {
    fn into(self) -> Inline {
        match self {
            OpenNode::Strong(inline) => Inline::Strong(inline),
            OpenNode::Emphsis(inline) => Inline::Emphasis(inline),
            OpenNode::Link { dst, content } => Inline::Link { dst, content },
            OpenNode::Strikethrough(inline) => Inline::Strikethrough(inline),
            _ => unreachable!(),
        }
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
        let block = match tag {
            Tag::Paragraph => OpenNode::Paragraph(Vec::new()),
            Tag::Heading { level, .. } => OpenNode::Heading {
                level: level as u8,
                content: Vec::new(),
            },
            Tag::CodeBlock(kind) => {
                let kind = match kind {
                    CodeBlockKind::Indented => Some("default".to_string()),
                    CodeBlockKind::Fenced(language) => Some(language.to_string()),
                };

                OpenNode::CodeBlock {
                    language: kind,
                    code: Vec::new(),
                }
            }
            Tag::Strong => OpenNode::Strong(Vec::new()),
            Tag::Emphasis => OpenNode::Emphsis(Vec::new()),
            Tag::Link { dest_url, .. } => OpenNode::Link {
                dst: dest_url.into_string(),
                content: Vec::new(),
            },
            Tag::Strikethrough => OpenNode::Strikethrough(Vec::new()),
            Tag::BlockQuote(_) => OpenNode::Quote(Vec::new()),
            Tag::List(list) => OpenNode::List {
                ordered: list.map(|e| true).unwrap_or_default(),
                items: Vec::new(),
            },
            Tag::Item => OpenNode::Item(Vec::new()),
            Tag::Table(_) => OpenNode::Table {
                headers: Vec::new(),
                rows: Vec::new(),
            },
            _ => OpenNode::Paragraph(Vec::new()),
        };

        self.stack.push(block);
    }

    fn should_finalize(&self) -> bool {
        self.stack.len() == 0
    }

    fn top_mut(&mut self) -> &mut OpenNode {
        self.stack.last_mut().unwrap()
    }

    fn append_inline(&mut self, node: Inline) {
        let last = self.top_mut();

        match last {
            OpenNode::Item(blocks) => match blocks.last_mut() {
                Some(MarkdownBlock::Paragraph(inlines)) => {
                    inlines.push(node);
                }
                _ => {
                    blocks.push(MarkdownBlock::Paragraph(vec![node]));
                }
            },
            OpenNode::Paragraph(inlines)
            | OpenNode::Strong(inlines)
            | OpenNode::Emphsis(inlines)
            | OpenNode::Strikethrough(inlines)
            | OpenNode::TableHeader(inlines)
            | OpenNode::TableRow(inlines)
            | OpenNode::CodeBlock { code: inlines, .. } => {
                inlines.push(node);
            }
            OpenNode::Heading { content, .. } | OpenNode::Link { content, .. } => {
                content.push(node);
            }
            _ => unreachable!(),
        }
    }

    fn append_block(&mut self, node: MarkdownBlock) {
        let last = self.top_mut();

        let parent = match last {
            OpenNode::List { items, .. } => {
                if items.is_empty() {
                    items.push(Vec::new());
                }
                items.last_mut().unwrap()
            }
            OpenNode::Item(items) => items,
            OpenNode::Quote(quotes) => quotes,
            _ => unreachable!(),
        };

        parent.push(node);
    }

    fn finalize_or_merge_last(&mut self) {
        let node = self.stack.pop().unwrap();

        if let OpenNode::Item(blocks) = node {
            let OpenNode::List { items, .. } = self.top_mut() else {
                unreachable!()
            };
            items.push(blocks);
            return;
        }

        if self.should_finalize() {
            self.parsed.push(node.into());
            return;
        }

        if node.is_inline() {
            self.append_inline(node.into());
            return;
        }

        self.append_block(node.into());
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

    fn finalize(self) -> Vec<MarkdownBlock> {
        self.parsed
    }

    fn plain_text(&mut self, text: impl Into<String>) {
        let inline = Inline::Text(text.into());

        match self.stack.last_mut() {
            Some(
                OpenNode::Paragraph(content)
                | OpenNode::Strong(content)
                | OpenNode::Emphsis(content)
                | OpenNode::Strikethrough(content)
                | OpenNode::TableHeader(content)
                | OpenNode::TableRow(content),
            ) => {
                content.push(inline);
            }
            Some(OpenNode::Heading { content, .. }) | Some(OpenNode::Link { content, .. }) => {
                content.push(inline);
            }
            Some(OpenNode::Item(blocks)) => match blocks.last_mut() {
                Some(MarkdownBlock::Paragraph(content)) => content.push(inline),
                _ => blocks.push(MarkdownBlock::Paragraph(vec![inline])),
            },
            Some(OpenNode::Quote(blocks)) => {
                blocks.push(MarkdownBlock::Paragraph(vec![inline]));
            }
            None => {
                self.parsed.push(MarkdownBlock::Paragraph(vec![inline]));
            }
            _ => {}
        }
    }
}

fn parse_markdown<'a>(markdown: &'a str) -> Vec<MarkdownBlock> {
    let mut options = Options::empty();

    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_FOOTNOTES);

    let mut state = State::new();

    let parser = Parser::new_ext(markdown.as_ref(), options);

    parser.into_offset_iter().for_each(|(event, offset)| {
        if let Some(unsupported) = &mut state.unsupported {
            match event {
                Event::Start(_) => unsupported.depth += 1,
                Event::End(_) => {
                    unsupported.depth -= 1;
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
            Event::End(_) => state.finalize_or_merge_last(),
            _ => state.plain_text(&markdown[offset]),
        }
    });

    state.finalize()
}

#[cfg(test)]
mod tests {
    use super::{Inline, MarkdownBlock, parse_markdown};

    #[test]
    fn parses_plain_paragraph() {
        let parsed = parse_markdown("hello");

        let [MarkdownBlock::Paragraph(content)] = parsed.as_slice() else {
            panic!("expected one paragraph, got {parsed:#?}");
        };
        let [Inline::Text(text)] = content.as_slice() else {
            panic!("expected one text inline, got {content:#?}");
        };

        assert_eq!(text, "hello");
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
    fn parses_tight_unordered_list_items_as_blocks() {
        let parsed = parse_markdown("- first\n- second");

        let [MarkdownBlock::List { ordered, items }] = parsed.as_slice() else {
            panic!("expected one list, got {parsed:#?}");
        };

        assert!(!ordered);
        assert_eq!(items.len(), 2);
        assert_list_item_text(&items[0], "first");
        assert_list_item_text(&items[1], "second");
    }

    #[test]
    fn parses_fenced_code_block_without_inline_markdown() {
        let parsed = parse_markdown("```rust\nfn main() {}\n```");

        let [MarkdownBlock::CodeBlock { language, code }] = parsed.as_slice() else {
            panic!("expected one code block, got {parsed:#?}");
        };

        assert_eq!(language.as_deref(), Some("rust"));

        let code = code
            .iter()
            .map(|e| match e {
                Inline::Text(text) => text.clone(),
                _ => unreachable!(),
            })
            .collect::<Vec<_>>()
            .join("");
        assert_eq!(code, "fn main() {}\n");
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

        let [MarkdownBlock::List { ordered, items }] = parsed.as_slice() else {
            panic!("expected one list, got {parsed:#?}");
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

        assert!(!ordered);
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
}
