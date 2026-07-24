use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    sync::Arc,
};

use iocraft::prelude::*;
use pulldown_cmark::{Event, Options, Parser, TagEnd};
use tokio::sync::{
    Mutex,
    mpsc::{Sender, UnboundedReceiver},
};

use crate::{
    event::AgentViewEvent,
    tool::{DisplayBlock, Presentation, ToolCallStatus},
    ui::markdown::MarkdownBlock,
};

mod markdown;

impl TryFrom<AgentViewEvent> for RenderUnit {
    type Error = anyhow::Error;

    fn try_from(value: AgentViewEvent) -> Result<Self, Self::Error> {
        match value {
            AgentViewEvent::TextDelta(_) => anyhow::bail!("must merge text delta"),
            AgentViewEvent::Tool(presentation) => Ok(RenderUnit::Tool(presentation)),
            AgentViewEvent::Completed => Ok(RenderUnit::Separator),
            AgentViewEvent::Err(e) => Ok(RenderUnit::Err(e)),
        }
    }
}

impl TryFrom<&AgentViewEvent> for RenderUnit {
    type Error = anyhow::Error;

    fn try_from(value: &AgentViewEvent) -> Result<Self, Self::Error> {
        value.clone().try_into()
    }
}

fn parse_units(
    events: &mut Vec<AgentViewEvent>,
    units: &mut Vec<RenderUnit>,
) -> anyhow::Result<()> {
    for unit in preprocess_events(events)? {
        match (units.last_mut(), &unit) {
            (Some(RenderUnit::Text(text)), RenderUnit::Text(next)) => text.push_str(next.as_str()),
            (Some(RenderUnit::Separator), RenderUnit::Separator) => {}
            (_, RenderUnit::Tool(presentation)) => {
                let current = units.iter_mut().find_map(|unit| match unit {
                    RenderUnit::Tool(current) if current.call_id == presentation.call_id => {
                        Some(current)
                    }
                    _ => None,
                });

                match current {
                    Some(current) => *current = presentation.clone(),
                    None => units.push(unit),
                }
            }
            _ => units.push(unit),
        }
    }

    events.clear();
    Ok(())
}

fn preprocess_events(events: &[AgentViewEvent]) -> anyhow::Result<Vec<RenderUnit>> {
    let mut units = Vec::new();
    let mut text = String::new();

    for event in events {
        match event {
            AgentViewEvent::TextDelta(delta) => text.push_str(delta),
            event => {
                if !text.is_empty() {
                    units.push(RenderUnit::Text(std::mem::take(&mut text)));
                }

                units.push(event.try_into()?);
            }
        }
    }

    if !text.is_empty() {
        units.push(RenderUnit::Text(text));
    }

    Ok(units)
}

#[derive(Debug, Clone)]
enum RenderUnit {
    Text(String),
    ParsedMarkdown(Vec<MarkdownBlock>),
    Tool(Presentation),
    Prompt(String),
    Separator,
    Err(String),
}

#[derive(Debug, Props, Default)]
pub struct UIProp {
    pub committer: Option<Sender<String>>,
    pub event_rx: Arc<Mutex<Option<UnboundedReceiver<AgentViewEvent>>>>,
}

#[component]
pub fn UI(mut hooks: Hooks, props: &UIProp) -> impl Into<AnyElement<'static>> {
    let mut units = hooks.use_state(|| Vec::<RenderUnit>::new());

    let event_rx = props.event_rx.clone();
    hooks.use_future(async move {
        tracing::info!(event = "ui.event_receiver.started");
        let mut rx = event_rx.lock().await.take().unwrap();

        while let Some(event) = rx.recv().await {
            let mut inner = units.write();
            if parse_units(&mut vec![event], &mut inner).is_err() {
                tracing::error!(
                    event = "ui.view_event.failed",
                    operation = "parse_units",
                    error_class = "view_event_parse_error"
                );
            }
        }

        tracing::info!(event = "ui.event_receiver.closed");
    });

    let committer = props.committer.clone();
    let input_handler = hooks.use_async_handler(move |s: String| {
        let committer = committer.clone().unwrap();

        Box::pin(async move {
            units.write().push(RenderUnit::Prompt(s.clone()));
            tracing::info!(event = "ui.prompt_submitted");
            if committer.send(s).await.is_err() {
                tracing::warn!(
                    event = "ui.prompt_send.failed",
                    operation = "prompt_channel_send",
                    error_class = "prompt_channel_closed"
                );
            }
        })
    });

    let (width, height) = hooks.use_terminal_size();

    element! {
        View(width: width, height: height, flex_direction: FlexDirection::Column) {
            View(width: 100pct, flex_grow: 1.0_f32, overflow: Overflow::Hidden) {
                ScrollView(
                    auto_scroll: false,
                    scrollbar: Some(false),
                    keyboard_scroll: Some(false),
                ) {
                    DisplayArea(units: units.read().iter().cloned().collect::<Vec<_>>())
                }
            }

            Textarea(on_submit: input_handler)
        }
    }
}

#[derive(Debug, Props, Default)]
struct RainbowTextProps {
    content: String,
    italic: bool,
}

#[component]
fn RainbowText(props: &RainbowTextProps) -> impl Into<AnyElement<'static>> {
    let mut hasher = DefaultHasher::new();
    props.content.hash(&mut hasher);
    let start_hue = (hasher.finish() % 360) as f32;
    let contents: Vec<MixedTextContent> = props
        .content
        .chars()
        .enumerate()
        .map(|(index, character)| {
            let mut content =
                MixedTextContent::new(character).color(get_rainbow_color(index, start_hue));

            if props.italic {
                content = content.italic();
            }

            content
        })
        .collect();

    element! {
        MixedText(contents: contents)
    }
}

fn get_rainbow_color(index: usize, start_hue: f32) -> Color {
    const SATURATION: f32 = 0.5;
    const BRIGHTNESS: f32 = 0.9;
    const HUE_STEP: f32 = 6.0;

    let hue = (start_hue + index as f32 * HUE_STEP) % 360.0;
    let (r, g, b) = hsv_to_rgb(hue, SATURATION, BRIGHTNESS);

    Color::Rgb { r, g, b }
}

fn hsv_to_rgb(hue: f32, saturation: f32, value: f32) -> (u8, u8, u8) {
    let chroma = value * saturation;
    let hue_section = hue / 60.0;
    let secondary = chroma * (1.0 - (hue_section % 2.0 - 1.0).abs());

    let (r, g, b) = match hue_section {
        section if section < 1.0 => (chroma, secondary, 0.0),
        section if section < 2.0 => (secondary, chroma, 0.0),
        section if section < 3.0 => (0.0, chroma, secondary),
        section if section < 4.0 => (0.0, secondary, chroma),
        section if section < 5.0 => (secondary, 0.0, chroma),
        _ => (chroma, 0.0, secondary),
    };

    let match_value = value - chroma;
    let to_byte = |component: f32| ((component + match_value) * 255.0).round() as u8;

    (to_byte(r), to_byte(g), to_byte(b))
}

#[derive(Debug, Default, Props)]
struct DisplayAreaProp {
    units: Vec<RenderUnit>,
}

#[derive(Props, Default)]
struct TextareaProp {
    on_submit: Handler<String>,
}

fn left_padding_to(s: impl AsRef<str>, min_len: usize) -> String {
    let s = s.as_ref();

    if s.len() >= min_len {
        return s.to_string();
    }

    format!("{}{}", " ".repeat(min_len - s.len()), s)
}

fn digits(n: usize) -> usize {
    n.checked_ilog10().unwrap_or(0) as usize + 1
}

fn format_code_lines(
    content: &str,
    truncated_lines: usize,
    show_line_numbers: bool,
    start_line_number: usize,
) -> Vec<String> {
    let lines = if content.is_empty() {
        vec![""]
    } else {
        content.lines().take(truncated_lines).collect::<Vec<_>>()
    };
    let last_line_number = start_line_number.saturating_add(lines.len().saturating_sub(1));
    let line_number_width = digits(last_line_number);

    lines
        .into_iter()
        .enumerate()
        .map(|(offset, line)| {
            if show_line_numbers {
                let line_number = start_line_number.saturating_add(offset);
                format!(
                    "     {} {}",
                    left_padding_to(line_number.to_string(), line_number_width),
                    line
                )
            } else {
                format!("     {line}")
            }
        })
        .collect()
}

fn render_tool<'a>(presentation: &Presentation) -> AnyElement<'a> {
    let indicator = match &presentation.status {
        ToolCallStatus::Running => "⟳ ",
        ToolCallStatus::Succeeded => "● ",
        ToolCallStatus::Failed { .. } => "✗ ",
    };

    let target = match &presentation.target {
        Some(target) => target.as_str(),
        None => "unknown",
    };

    let title = format!(
        "{} {}({}) <- ({})",
        indicator, presentation.name, target, presentation.label
    );

    let title = element! {
        RainbowText(content: title)
    }
    .into_any();

    let mut blocks = Vec::new();

    for block in &presentation.blocks {
        match block {
            DisplayBlock::Summary(summary) => {
                blocks.push(element! { Text(content: format!("   └ {summary}")) }.into_any())
            }
            DisplayBlock::TextOutput {
                content,
                truncated_lines: _,
            } => blocks.push(element! { Text(content: format!("   └ {content}")) }.into_any()),
            DisplayBlock::CodeBlock {
                language: _,
                content,
                truncated_lines,
                show_line_numbers,
                start_line_number,
            } => {
                blocks.extend(
                    format_code_lines(
                        content,
                        *truncated_lines,
                        *show_line_numbers,
                        *start_line_number,
                    )
                    .into_iter()
                    .map(|content| element! { Text(content: content) }.into_any()),
                );
            }
            DisplayBlock::KeyValue { entries } => blocks.extend(
                entries
                    .iter()
                    .map(|entry| {
                        element! { Text(content: format!("     - {}: {}", entry.key, entry.value)) }
                            .into_any()
                    })
                    .collect::<Vec<_>>(),
            ),
            _ => todo!(),
        }
    }

    element! {
        View(width: 100pct, height: blocks.len() as u16, flex_direction: FlexDirection::Column) {
            #(title)
            #(blocks.into_iter())
        }
    }
    .into_any()
}

#[component]
fn DisplayArea<'a>(mut hooks: Hooks, props: &DisplayAreaProp) -> impl Into<AnyElement<'a>> {
    let (width, _) = hooks.use_terminal_size();

    element! {
        View(width: 100pct, flex_direction: FlexDirection::Column, row_gap: 1) {
            #(props.units.iter().map(|unit| {
                match unit {
                    RenderUnit::Text(text) => element! { Text(content: format!("{}", text.as_str()), color: Some(Color::Cyan)) }.into_any(),
                    RenderUnit::Prompt(text) => element! { RainbowText(content: format!("❯ {}", text), italic: true) }.into_any(),
                    RenderUnit::ParsedMarkdown(_) => todo!(),
                    RenderUnit::Separator => element! { Text(content: "─".repeat(width as usize).to_string()) }.into_any(),
                    RenderUnit::Tool(presentation) => render_tool(presentation),
                    RenderUnit::Err(err) => element! { Text(content: format!("✖ {err}"), color: Some(Color::Red)) }.into_any(),
                }
            }))
        }
    }
}

fn textarea_height(input: impl AsRef<str>, width: u16) -> u16 {
    const PROMPT_WIDTH: usize = 2;
    const CURSOR_WIDTH: usize = 1;
    const BORDER_HEIGHT: usize = 2;

    const MIN_HEIGHT: usize = 3;
    const MAX_HEIGHT: usize = 10;

    let text_width = usize::from(width)
        .saturating_sub(PROMPT_WIDTH)
        .saturating_sub(CURSOR_WIDTH)
        .max(1);

    let lines = input
        .as_ref()
        .split('\n')
        .map(|line| {
            if line.is_empty() {
                1
            } else {
                textwrap::wrap(line, text_width).len().max(1)
            }
        })
        .sum::<usize>();

    lines
        .saturating_add(BORDER_HEIGHT)
        .clamp(MIN_HEIGHT, MAX_HEIGHT) as u16
}

#[component]
fn Textarea<'a>(mut hooks: Hooks, props: &TextareaProp) -> impl Into<AnyElement<'a>> {
    let mut input = hooks.use_state(|| "".to_string());

    let (width, _) = hooks.use_terminal_size();

    let height = textarea_height(input.read().as_str(), width);

    let on_submit = props.on_submit.clone();
    hooks.use_local_terminal_events(move |event| {
        if let TerminalEvent::Key(key) = event {
            if key.code == KeyCode::Enter && key.kind == KeyEventKind::Press {
                let value = input.read().clone();

                if !value.trim().is_empty() {
                    on_submit(input.read().clone());
                }

                input.set("".to_string());
            }
        }
    });

    element! {
        View(width: 100pct, height: height, border_style: BorderStyle::Round, border_edges: Some(Edges::Top | Edges::Bottom)) {
            View(width: 2) {
                Text(content: "❯ ".to_string())
            }

            View(flex_grow: 1.0f32) {
                TextInput(
                    has_focus: true,
                    value: input.to_string(),
                    on_change: move |new_value| {
                        input.set(new_value)
                    },
                    multiline: true,
                    italic: true,
                )
            }
        }
    }
}

#[cfg(test)]
mod view_event_tests {
    use super::*;
    use crate::tool::{ToolCallId, ToolCallStatus};

    fn presentation(status: ToolCallStatus) -> Presentation {
        Presentation {
            call_id: ToolCallId("call-1".to_owned()),
            name: "Test Tool".to_owned(),
            label: "tool".to_owned(),
            target: None,
            status,
            blocks: Vec::new(),
        }
    }

    #[test]
    fn merges_text_and_upserts_tool_presentations() {
        let mut units = Vec::new();
        let mut events = vec![
            AgentViewEvent::TextDelta("hello ".to_owned()),
            AgentViewEvent::TextDelta("world".to_owned()),
            AgentViewEvent::Tool(presentation(ToolCallStatus::Running)),
        ];

        parse_units(&mut events, &mut units).unwrap();

        assert!(matches!(&units[0], RenderUnit::Text(text) if text == "hello world"));
        assert!(matches!(
            &units[1],
            RenderUnit::Tool(tool) if matches!(tool.status, ToolCallStatus::Running)
        ));

        let mut events = vec![AgentViewEvent::Tool(presentation(
            ToolCallStatus::Succeeded,
        ))];
        parse_units(&mut events, &mut units).unwrap();

        assert_eq!(units.len(), 2);
        assert!(matches!(
            &units[1],
            RenderUnit::Tool(tool) if matches!(tool.status, ToolCallStatus::Succeeded)
        ));
    }

    #[test]
    fn formats_absolute_code_line_numbers() {
        assert_eq!(
            format_code_lines("alpha\nbeta\ngamma", 10, true, 998),
            vec![
                "      998 alpha".to_owned(),
                "      999 beta".to_owned(),
                "     1000 gamma".to_owned(),
            ]
        );
    }

    #[test]
    fn formats_a_numbered_blank_code_line() {
        assert_eq!(
            format_code_lines("", 10, true, 42),
            vec!["     42 ".to_owned()]
        );
    }

    #[test]
    fn formats_code_without_line_numbers_and_honors_limit() {
        assert_eq!(
            format_code_lines("alpha\nbeta\ngamma", 2, false, 42),
            vec!["     alpha".to_owned(), "     beta".to_owned()]
        );
    }

    #[test]
    fn collapses_repeated_completion_separators() {
        let mut units = Vec::new();
        let mut events = vec![AgentViewEvent::Completed, AgentViewEvent::Completed];

        parse_units(&mut events, &mut units).unwrap();

        assert_eq!(units.len(), 1);
        assert!(matches!(units[0], RenderUnit::Separator));
    }
}
