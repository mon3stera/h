use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    sync::Arc,
    time::Duration,
};

use iocraft::{hooks, prelude::*};
use tokio::{
    sync::{
        Mutex,
        mpsc::{Receiver, Sender, UnboundedReceiver},
    },
    time::{MissedTickBehavior, interval},
};

use crate::{
    event::{AgentViewEvent, UiRequest},
    tool::{DiffLine, DiffLineKind, DisplayBlock, Presentation, ToolCallId, ToolCallStatus},
    ui::{
        banner::render_banner,
        markdown::{MarkdownBlock, parse_markdown},
        markdown_view::render_markdown,
    },
};

mod banner;
pub mod choice_list;
pub mod markdown;
mod markdown_view;
pub mod resume;

impl TryFrom<AgentViewEvent> for RenderUnit {
    type Error = anyhow::Error;

    fn try_from(value: AgentViewEvent) -> Result<Self, Self::Error> {
        match value {
            AgentViewEvent::Startup { .. } => anyhow::bail!("must update startup state"),
            AgentViewEvent::TextDelta(_) => anyhow::bail!("must merge text delta"),
            AgentViewEvent::TurnStart | AgentViewEvent::TurnFinished => {
                anyhow::bail!("should not be rendered")
            }
            AgentViewEvent::Prompt(prompt) => Ok(RenderUnit::Prompt(prompt)),
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
        match unit {
            RenderUnit::Text(next) => match units.last_mut() {
                Some(RenderUnit::Text(text)) => text.push_str(&next),
                _ => units.push(RenderUnit::Text(next)),
            },
            RenderUnit::Separator => {
                finalize_response_markdown(units);
                if !matches!(units.last(), Some(RenderUnit::Separator)) {
                    units.push(RenderUnit::Separator);
                }
            }
            RenderUnit::Tool(presentation) => {
                let current = units.iter_mut().find_map(|unit| match unit {
                    RenderUnit::Tool(current) if current.call_id == presentation.call_id => {
                        Some(current)
                    }
                    _ => None,
                });

                match current {
                    Some(current) => *current = presentation,
                    None => units.push(RenderUnit::Tool(presentation)),
                }
            }
            unit => units.push(unit),
        }
    }

    events.clear();
    Ok(())
}

fn finalize_response_markdown(units: &mut [RenderUnit]) {
    let response_start = units
        .iter()
        .rposition(|unit| matches!(unit, RenderUnit::Separator))
        .map_or(0, |index| index + 1);

    for unit in &mut units[response_start..] {
        if let RenderUnit::Text(source) = unit {
            let source = std::mem::take(source);
            *unit = RenderUnit::ParsedMarkdown(parse_markdown(&source));
        }
    }
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct StartupInfo {
    model: String,
    thinking_effort: Option<String>,
}

#[derive(Debug, Default)]
struct ViewState {
    startup: Option<StartupInfo>,
    units: Vec<RenderUnit>,
    turn_in_progress: bool,
}

fn reduce_view_event(state: &mut ViewState, event: AgentViewEvent) -> anyhow::Result<()> {
    match event {
        AgentViewEvent::Startup {
            model,
            thinking_effort,
        } => {
            state.startup = Some(StartupInfo {
                model,
                thinking_effort,
            });
            Ok(())
        }
        AgentViewEvent::TurnStart => {
            state.turn_in_progress = true;
            Ok(())
        }
        AgentViewEvent::TurnFinished => {
            state.turn_in_progress = false;
            Ok(())
        }
        event => parse_units(&mut vec![event], &mut state.units),
    }
}

#[derive(Debug, Props, Default)]
pub struct UIProp {
    pub committer: Option<Sender<String>>,
    pub event_rx: Arc<Mutex<Option<UnboundedReceiver<AgentViewEvent>>>>,
    /// Questions the agent is waiting on. Drain this the way `event_rx` is
    /// drained, queue what arrives in `ViewState`, and reply through each
    /// request's `oneshot::Sender`.
    pub ui_request_rx: Arc<Mutex<Option<Receiver<UiRequest>>>>,
}

#[component]
pub fn UI(mut hooks: Hooks, props: &UIProp) -> impl Into<AnyElement<'static>> {
    let mut state = hooks.use_state(ViewState::default);

    let event_rx = props.event_rx.clone();
    hooks.use_future(async move {
        tracing::info!(event = "ui.event_receiver.started");
        let mut rx = event_rx.lock().await.take().unwrap();

        while let Some(event) = rx.recv().await {
            if reduce_view_event(&mut state.write(), event).is_err() {
                tracing::error!(
                    event = "ui.view_event.failed",
                    operation = "reduce_view_event",
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
            state.write().units.push(RenderUnit::Prompt(s.clone()));
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
    let state = state.read();

    let indicator = state
        .turn_in_progress
        .then(|| element! { WorkingIndicator });

    element! {
        View(width: width, height: height, flex_direction: FlexDirection::Column) {
            View(width: 100pct, flex_grow: 1.0_f32, overflow: Overflow::Hidden) {
                ScrollView(
                    auto_scroll: true,
                    scrollbar: Some(false),
                    keyboard_scroll: Some(false),
                ) {
                    DisplayArea(
                        width,
                        startup: state.startup.clone(),
                        units: state.units.clone(),
                    )
                }
            }
            #(indicator)
            Textarea(on_submit: input_handler, turn_in_progress: state.turn_in_progress)
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
    width: u16,
    startup: Option<StartupInfo>,
    units: Vec<RenderUnit>,
}

#[derive(Props, Default)]
struct TextareaProp {
    on_submit: Handler<String>,
    turn_in_progress: bool,
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

/// Diff row washes, dark enough that the default foreground stays legible on top
/// of them. A saturated fill reads as a block of colour rather than as code.
const REMOVED_WASH: Color = Color::Rgb {
    r: 0x4a,
    g: 0x1e,
    b: 0x24,
};
const ADDED_WASH: Color = Color::Rgb {
    r: 0x1c,
    g: 0x3d,
    b: 0x28,
};

/// Lays out one diff line as `<number> <sign><text>` and gives back the
/// background to wash the row in: red for a removal, green for an addition,
/// nothing for context.
///
/// The colour is a background rather than a foreground because the row is painted
/// edge to edge; tinting the glyphs the same hue would leave them unreadable
/// against it.
///
/// `width` is shared by every line of a diff so the numbers, and the code after
/// them, stay in a column.
fn format_diff_line(line: &DiffLine, width: usize) -> (String, Option<Color>) {
    let (sign, background) = match line.kind {
        DiffLineKind::Removed => ('-', Some(REMOVED_WASH)),
        DiffLineKind::Added => ('+', Some(ADDED_WASH)),
        DiffLineKind::Context => (' ', None),
    };

    (
        format!("     {:>width$} {sign}{}", line.number, line.text),
        background,
    )
}

fn diff_number_width(lines: &[DiffLine]) -> usize {
    lines
        .iter()
        .map(|line| line.number)
        .max()
        .unwrap_or(0)
        .to_string()
        .len()
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
            DisplayBlock::Diff { lines } => {
                let width = diff_number_width(lines);

                blocks.extend(lines.iter().map(|line| {
                    let (content, background) = format_diff_line(line, width);

                    // The wrapper is what carries the colour: `Text` has no
                    // background, and only a full-width box washes the whole row
                    // rather than just the glyphs.
                    element! {
                        View(width: 100pct, background_color: background) {
                            Text(content: content)
                        }
                    }
                    .into_any()
                }));
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
        View(width: 100pct, flex_direction: FlexDirection::Column) {
            #(title)
            #(blocks.into_iter())
        }
    }
    .into_any()
}

/// Tools whose whole effect is to look at something. A run of them collapses into
/// one Explore block: a reader usually cares that exploring happened, not about
/// each file it touched.
const EXPLORATORY_TOOLS: [&str; 3] = ["ReadFile", "Grep", "Fetch"];

/// Up to this many distinct targets are named; beyond it the group reports a
/// count, so a long run of reads stays as compact as a short one.
const NAMED_TARGET_LIMIT: usize = 3;

/// What a tool counts when there are too many targets to name.
fn counted_noun(tool: &str, count: usize) -> String {
    let noun = match tool {
        "ReadFile" => "file",
        "Grep" => "path",
        "Fetch" => "url",
        _ => "call",
    };

    match count {
        1 => format!("1 {noun}"),
        _ => format!("{count} {noun}s"),
    }
}

/// Only finished exploration collapses. A running tool keeps its own row so its
/// spinner stays visible, and a failed one keeps its own row so the failure is
/// not buried inside a count.
fn is_collapsible_exploration(presentation: &Presentation) -> bool {
    matches!(presentation.status, ToolCallStatus::Succeeded)
        && EXPLORATORY_TOOLS.contains(&presentation.name.as_str())
}

/// Folds a run of exploratory presentations into one synthetic presentation, so
/// the ordinary tool rendering draws it — title, glyphs and all.
fn explore_presentation(run: &[&Presentation]) -> Presentation {
    let mut tools: Vec<(&str, Vec<&str>)> = Vec::new();

    for presentation in run {
        let targets = match tools
            .iter_mut()
            .find(|(name, _)| *name == presentation.name.as_str())
        {
            Some((_, targets)) => targets,
            None => {
                tools.push((presentation.name.as_str(), Vec::new()));
                &mut tools.last_mut().expect("just pushed").1
            }
        };

        // The same file read twice is one file, not two.
        if let Some(target) = presentation.target.as_deref() {
            if !targets.contains(&target) {
                targets.push(target);
            }
        }
    }

    let blocks = tools
        .into_iter()
        .map(|(name, targets)| {
            let detail = if targets.is_empty() {
                counted_noun(name, 0)
            } else if targets.len() <= NAMED_TARGET_LIMIT {
                targets.join(", ")
            } else {
                counted_noun(name, targets.len())
            };

            DisplayBlock::Summary(format!("{name} {detail}"))
        })
        .collect();

    Presentation {
        call_id: run
            .first()
            .map(|presentation| presentation.call_id.clone())
            .unwrap_or_else(|| ToolCallId(String::new())),
        name: "Explore".to_owned(),
        label: "aggregator".to_owned(),
        target: Some(counted_noun("", run.len())),
        status: ToolCallStatus::Succeeded,
        blocks,
    }
}

/// A unit as it will be drawn: on its own, as a single tool, or as a run of
/// exploration folded together.
enum RenderGroup<'a> {
    Unit(&'a RenderUnit),
    Tool(&'a Presentation),
    Explore(Vec<&'a Presentation>),
}

/// Collapses each run of two or more finished exploratory tools. A lone read is
/// left alone — naming the one file it touched says more than "1 file" would.
fn group_units(units: &[RenderUnit]) -> Vec<RenderGroup<'_>> {
    let mut groups: Vec<RenderGroup<'_>> = Vec::new();

    for unit in units {
        // Each tool round ends with one, so reads arrive with separators between
        // them. Nothing draws them, so nothing should be split by them either.
        if matches!(unit, RenderUnit::Separator) {
            continue;
        }

        let exploring = match unit {
            RenderUnit::Tool(presentation) if is_collapsible_exploration(presentation) => {
                Some(presentation)
            }
            _ => None,
        };

        match (exploring, groups.last_mut()) {
            (Some(presentation), Some(RenderGroup::Explore(run))) => run.push(presentation),
            (Some(presentation), _) => groups.push(RenderGroup::Explore(vec![presentation])),
            (None, _) => groups.push(RenderGroup::Unit(unit)),
        }
    }

    // A run of one was never a run; give the tool its own detailed row back.
    for group in &mut groups {
        if let RenderGroup::Explore(run) = group {
            if let [only] = run.as_slice() {
                *group = RenderGroup::Tool(only);
            }
        }
    }

    groups
}

#[component]
fn DisplayArea<'a>(props: &DisplayAreaProp) -> impl Into<AnyElement<'a>> {
    let banner = props.startup.as_ref().map(|startup| {
        render_banner(
            &startup.model,
            startup.thinking_effort.as_deref(),
            props.width,
        )
    });

    element! {
        View(width: 100pct, flex_direction: FlexDirection::Column, row_gap: 1) {
            #(banner)
            #(group_units(&props.units).into_iter().filter_map(|group| {
                let unit = match group {
                    RenderGroup::Tool(presentation) => return Some(render_tool(presentation)),
                    RenderGroup::Explore(run) => {
                        return Some(render_tool(&explore_presentation(&run)));
                    }
                    RenderGroup::Unit(unit) => unit,
                };

                Some(match unit {
                    RenderUnit::Text(text) => render_markdown(&parse_markdown(text)),
                    RenderUnit::Prompt(text) => element! { RainbowText(content: format!("❯ {}", text), italic: true) }.into_any(),
                    RenderUnit::ParsedMarkdown(blocks) => render_markdown(blocks),
                    // `group_units` already drops these; the arm keeps the match
                    // exhaustive. Separators stay in the unit list only to mark
                    // where a response begins for `finalize_response_markdown`.
                    RenderUnit::Separator => return None,
                    RenderUnit::Tool(presentation) => render_tool(presentation),
                    RenderUnit::Err(err) => element! { Text(content: format!("✖ {err}"), color: Some(Color::Red)) }.into_any(),
                })
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
    let in_progress = props.turn_in_progress;
    hooks.use_local_terminal_events(move |event| {
        if let TerminalEvent::Key(key) = event {
            // A modified Enter submits; a bare one is left to the text input,
            // which takes it as a newline.
            //
            // Both chords are accepted because they are not equally available.
            // Ctrl+Enter reaches us only over the kitty keyboard protocol —
            // without it, terminals send a bare CR for Ctrl+Enter, identical to
            // Enter. Alt+Enter arrives as an ESC-prefixed CR, which nearly every
            // terminal sends, so it always works.
            if key.code == KeyCode::Enter
                && key.kind == KeyEventKind::Press
                && key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            {
                let value = input.read().clone();

                if !in_progress && !value.trim().is_empty() {
                    on_submit(input.read().clone());
                    input.set("".to_string());
                }
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

const WORKING_FRAMES: [(&str, &str); 4] = [
    ("◜", "h-..."),
    ("◝", "h-i..."),
    ("◞", "h-in..."),
    ("◟", "h-ing..."),
];

#[component]
fn WorkingIndicator(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut frame = hooks.use_state(|| 0_usize);

    hooks.use_future(async move {
        let mut timer = interval(Duration::from_millis(200));
        timer.set_missed_tick_behavior(MissedTickBehavior::Skip);

        timer.tick().await;

        loop {
            timer.tick().await;

            let next = (*frame.read() + 1) % WORKING_FRAMES.len();
            frame.set(next);
        }
    });

    let (arc, message) = WORKING_FRAMES[*frame.read()];

    element! {
        View(width: 100pct, height: 1, flex_direction: FlexDirection::Row) {
            RainbowText(content: format!("  {arc} {message}"))
        }
    }
}

#[cfg(test)]
mod view_event_tests {
    use super::*;
    use crate::{
        bridge::UiBridge,
        tool::{ToolCallId, ToolCallStatus},
    };

    fn explored(name: &str, target: &str) -> RenderUnit {
        tool_unit(name, target, ToolCallStatus::Succeeded)
    }

    fn tool_unit(name: &str, target: &str, status: ToolCallStatus) -> RenderUnit {
        RenderUnit::Tool(Presentation {
            call_id: ToolCallId(format!("{name}-{target}")),
            name: name.to_owned(),
            label: "built-in".to_owned(),
            target: Some(target.to_owned()),
            status,
            blocks: Vec::new(),
        })
    }

    /// The `└ ` summary lines an Explore group would draw, and its title target.
    fn explore_of(units: &[RenderUnit]) -> (String, Vec<String>) {
        let groups = group_units(units);

        let [RenderGroup::Explore(run)] = groups.as_slice() else {
            panic!(
                "expected exactly one Explore group, got {} groups",
                groups.len()
            );
        };

        let folded = explore_presentation(run);
        let summaries = folded
            .blocks
            .iter()
            .map(|block| match block {
                DisplayBlock::Summary(summary) => summary.clone(),
                other => panic!("expected summaries, got {other:?}"),
            })
            .collect();

        (folded.target.clone().unwrap_or_default(), summaries)
    }

    /// Reproduces the live sequence: each tool round ends with `Completed`, so
    /// two reads arrive with a separator between them.
    #[test]
    fn a_run_of_reads_folds_across_the_separators_between_rounds() {
        let mut state = ViewState::default();

        for (index, target) in ["a.rs", "b.rs", "c.rs"].into_iter().enumerate() {
            let RenderUnit::Tool(mut presentation) = explored("ReadFile", target) else {
                unreachable!("explored builds a tool unit")
            };
            presentation.call_id = ToolCallId(format!("call-{index}"));

            reduce_view_event(&mut state, AgentViewEvent::Tool(presentation)).unwrap();
            reduce_view_event(&mut state, AgentViewEvent::Completed).unwrap();
        }

        let groups = group_units(&state.units);

        assert!(
            matches!(groups.as_slice(), [RenderGroup::Explore(run)] if run.len() == 3),
            "separators are not drawn, so they must not break a run either: {} groups",
            groups.len()
        );
    }

    #[test]
    fn a_run_of_reads_folds_into_one_explore_group() {
        let (target, summaries) = explore_of(&[
            explored("ReadFile", "src/agent.rs"),
            explored("ReadFile", "src/ui.rs"),
            explored("Grep", "src/"),
        ]);

        assert_eq!(target, "3 calls");
        assert_eq!(
            summaries,
            ["ReadFile src/agent.rs, src/ui.rs", "Grep src/"],
            "each tool gets a line, in the order it first appeared"
        );
    }

    #[test]
    fn many_targets_are_counted_instead_of_named() {
        let (_, summaries) = explore_of(&[
            explored("ReadFile", "a.rs"),
            explored("ReadFile", "b.rs"),
            explored("ReadFile", "c.rs"),
            explored("ReadFile", "d.rs"),
        ]);

        assert_eq!(summaries, ["ReadFile 4 files"], "over the naming limit");
    }

    #[test]
    fn the_naming_limit_is_inclusive() {
        let (_, summaries) = explore_of(&[
            explored("ReadFile", "a.rs"),
            explored("ReadFile", "b.rs"),
            explored("ReadFile", "c.rs"),
        ]);

        assert_eq!(summaries, ["ReadFile a.rs, b.rs, c.rs"]);
    }

    #[test]
    fn each_tool_counts_in_its_own_terms() {
        let (_, summaries) = explore_of(&[
            explored("Grep", "a"),
            explored("Grep", "b"),
            explored("Grep", "c"),
            explored("Grep", "d"),
            explored("Fetch", "http://1"),
            explored("Fetch", "http://2"),
            explored("Fetch", "http://3"),
            explored("Fetch", "http://4"),
        ]);

        assert_eq!(summaries, ["Grep 4 paths", "Fetch 4 urls"]);
    }

    #[test]
    fn the_same_file_read_twice_counts_once() {
        let (target, summaries) = explore_of(&[
            explored("ReadFile", "same.rs"),
            explored("ReadFile", "same.rs"),
        ]);

        assert_eq!(target, "2 calls", "the calls really did happen");
        assert_eq!(summaries, ["ReadFile same.rs"], "but it is one file");
    }

    #[test]
    fn a_lone_read_keeps_its_own_row() {
        let units = [explored("ReadFile", "src/agent.rs")];
        let groups = group_units(&units);

        assert!(
            matches!(groups.as_slice(), [RenderGroup::Tool(_)]),
            "naming the one file says more than \"1 file\" would"
        );
    }

    #[test]
    fn a_writing_tool_breaks_a_run_in_two() {
        let units = [
            explored("ReadFile", "a.rs"),
            explored("ReadFile", "b.rs"),
            tool_unit("Edit", "a.rs", ToolCallStatus::Succeeded),
            explored("ReadFile", "c.rs"),
            explored("ReadFile", "d.rs"),
        ];
        let groups = group_units(&units);

        assert!(
            matches!(
                groups.as_slice(),
                [
                    RenderGroup::Explore(first),
                    RenderGroup::Unit(_),
                    RenderGroup::Explore(second),
                ] if first.len() == 2 && second.len() == 2
            ),
            "an edit is not exploration and must stay where it happened"
        );
    }

    #[test]
    fn an_unfinished_or_failed_read_stays_visible_on_its_own() {
        for status in [
            ToolCallStatus::Running,
            ToolCallStatus::Failed {
                message: "denied".to_owned(),
            },
        ] {
            let units = [
                explored("ReadFile", "a.rs"),
                explored("ReadFile", "b.rs"),
                tool_unit("ReadFile", "c.rs", status.clone()),
            ];
            let groups = group_units(&units);

            assert!(
                matches!(
                    groups.as_slice(),
                    [RenderGroup::Explore(run), RenderGroup::Unit(_)] if run.len() == 2
                ),
                "{status:?} must not be buried in a count: {} groups",
                groups.len()
            );
        }
    }

    #[test]
    fn prose_between_reads_keeps_them_apart() {
        let units = [
            explored("ReadFile", "a.rs"),
            RenderUnit::Text("thinking".to_owned()),
            explored("ReadFile", "b.rs"),
        ];
        let groups = group_units(&units);

        assert_eq!(groups.len(), 3, "neither read has a neighbour to fold with");
        assert!(matches!(
            groups.as_slice(),
            [
                RenderGroup::Tool(_),
                RenderGroup::Unit(RenderUnit::Text(_)),
                RenderGroup::Tool(_),
            ]
        ));
    }

    #[test]
    #[ignore = "timing probe, run explicitly"]
    fn probe_render_cost_by_unit_count() {
        use std::time::Instant;

        let prose = "Some explanatory prose about the change.\n\nWith a second paragraph and a `code` span.";

        let time = |label: &str, units: Vec<RenderUnit>| {
            let count = units.len();
            // Warm up, then measure a few frames.
            let mut element = element! {
                DisplayArea(width: 100_u16, startup: None, units: units)
            };
            element.render(Some(100));

            let started = Instant::now();
            for _ in 0..10 {
                element.render(Some(100));
            }
            let per_frame = started.elapsed() / 10;
            println!("PROBE {label:<28} n={count:<5} {per_frame:?}/frame");
        };

        for n in [50_usize, 200, 500, 1000] {
            time(
                "parsed markdown units",
                (0..n)
                    .map(|_| RenderUnit::ParsedMarkdown(parse_markdown(prose)))
                    .collect(),
            );
        }

        for n in [50_usize, 200, 500] {
            time(
                "unparsed text units",
                (0..n).map(|_| RenderUnit::Text(prose.to_owned())).collect(),
            );
        }

        for n in [50_usize, 200, 500] {
            time(
                "tool units",
                (0..n)
                    .map(|index| explored("ReadFile", &format!("src/file{index}.rs")))
                    .collect(),
            );
        }

        const N: usize = 500;

        time(
            "paragraph, 5 chars",
            (0..N)
                .map(|_| RenderUnit::ParsedMarkdown(parse_markdown("hello")))
                .collect(),
        );
        time(
            "paragraph, 400 chars",
            (0..N)
                .map(|_| RenderUnit::ParsedMarkdown(parse_markdown(&"word ".repeat(80))))
                .collect(),
        );
        time(
            "code block, 20 lines",
            (0..N)
                .map(|_| {
                    RenderUnit::ParsedMarkdown(parse_markdown(&format!(
                        "```rust\n{}```",
                        "let x = 1;\n".repeat(20)
                    )))
                })
                .collect(),
        );
        time(
            "prompt (RainbowText)",
            (0..N)
                .map(|_| RenderUnit::Prompt("a prompt".to_owned()))
                .collect(),
        );
        time(
            "raw text, no markdown",
            (0..N)
                .map(|_| RenderUnit::ParsedMarkdown(Vec::new()))
                .collect(),
        );
    }

    fn diff_line(number: usize, kind: DiffLineKind, text: &str) -> DiffLine {
        DiffLine {
            number,
            kind,
            text: text.to_owned(),
        }
    }

    #[test]
    fn a_diff_line_puts_its_number_before_the_sign() {
        let (content, background) =
            format_diff_line(&diff_line(1048, DiffLineKind::Added, "    let x = 1;"), 4);

        assert_eq!(content, "     1048 +    let x = 1;");
        assert_eq!(background, Some(ADDED_WASH));
    }

    #[test]
    fn diff_lines_are_washed_by_their_kind() {
        let background = |kind| format_diff_line(&diff_line(1, kind, "x"), 1).1;

        assert_eq!(background(DiffLineKind::Removed), Some(REMOVED_WASH));
        assert_eq!(background(DiffLineKind::Added), Some(ADDED_WASH));
        assert_eq!(background(DiffLineKind::Context), None);
    }

    /// The kind decides the colour, so a context line that reads like a removal
    /// still renders as context.
    #[test]
    fn a_context_line_beginning_with_a_dash_stays_unwashed() {
        let (content, background) =
            format_diff_line(&diff_line(7, DiffLineKind::Context, "---"), 1);

        assert_eq!(content, "     7  ---");
        assert_eq!(background, None);
    }

    /// The colour has to land on the cells, not the glyphs, and has to reach the
    /// end of the row even where the text stops short.
    #[test]
    fn a_changed_row_is_washed_edge_to_edge() {
        const WIDTH: usize = 40;

        let presentation = Presentation {
            call_id: ToolCallId("call-1".to_owned()),
            name: "Edit".to_owned(),
            label: "built-in".to_owned(),
            target: Some("src/main.rs".to_owned()),
            status: ToolCallStatus::Succeeded,
            blocks: vec![DisplayBlock::Diff {
                lines: vec![
                    diff_line(10, DiffLineKind::Removed, "old"),
                    diff_line(10, DiffLineKind::Added, "new"),
                    diff_line(11, DiffLineKind::Context, "kept"),
                ],
            }],
        };

        // Wrapped in a fixed-width parent the way the live tree wraps it: a
        // percentage width has nothing to resolve against on its own.
        let canvas = element! {
            View(width: WIDTH as u16) {
                #(render_tool(&presentation))
            }
        }
        .render(Some(WIDTH));

        let backgrounds = |row: usize| {
            (0..WIDTH)
                .map(|column| {
                    canvas
                        .cell(column, row)
                        .and_then(|cell| cell.background_color)
                })
                .collect::<Vec<_>>()
        };

        // Row 0 is the tool title; the diff starts under it.
        assert_eq!(
            backgrounds(1),
            vec![Some(REMOVED_WASH); WIDTH],
            "a removal is red across the whole row"
        );
        assert_eq!(
            backgrounds(2),
            vec![Some(ADDED_WASH); WIDTH],
            "an addition is green across the whole row"
        );
        assert_eq!(
            backgrounds(3),
            vec![None; WIDTH],
            "context keeps the default background"
        );
    }

    #[test]
    fn the_widest_number_sets_the_column_for_every_line() {
        let lines = vec![
            diff_line(9, DiffLineKind::Context, "nine"),
            diff_line(10, DiffLineKind::Removed, "ten"),
        ];
        let width = diff_number_width(&lines);

        assert_eq!(width, 2);
        assert_eq!(format_diff_line(&lines[0], width).0, "      9  nine");
        assert_eq!(format_diff_line(&lines[1], width).0, "     10 -ten");
    }

    /// Shutdown hangs on this: the agent worker archives once every prompt
    /// sender is gone, so the UI must not leave one behind when it quits.
    #[tokio::test]
    async fn quitting_the_ui_releases_the_prompt_sender() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
        let (_bridge, ui_request_rx) = UiBridge::new();
        let (_view_tx, view_rx) = tokio::sync::mpsc::unbounded_channel();

        let mut ui = element!(UI(
            committer: Some(tx),
            event_rx: Arc::new(Mutex::new(Some(view_rx))),
            ui_request_rx: Arc::new(Mutex::new(Some(ui_request_rx))),
        ));

        // Mount it once, so the component clones the sender into its input
        // handler the way a live session does.
        ui.render(Some(80));
        drop(ui);

        assert!(
            rx.recv().await.is_none(),
            "a sender outlived the UI, so the worker would wait forever"
        );
    }

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
    fn startup_event_updates_banner_state_without_adding_a_render_unit() {
        let mut state = ViewState::default();

        reduce_view_event(
            &mut state,
            AgentViewEvent::Startup {
                model: "gpt-5.6-sol".to_owned(),
                thinking_effort: Some("high".to_owned()),
            },
        )
        .unwrap();

        assert_eq!(
            state.startup,
            Some(StartupInfo {
                model: "gpt-5.6-sol".to_owned(),
                thinking_effort: Some("high".to_owned()),
            })
        );
        assert!(state.units.is_empty());
    }

    #[test]
    fn startup_state_survives_content_and_is_replaced_by_updates() {
        let mut state = ViewState::default();
        reduce_view_event(
            &mut state,
            AgentViewEvent::Startup {
                model: "first-model".to_owned(),
                thinking_effort: None,
            },
        )
        .unwrap();
        state.units.push(RenderUnit::Prompt("hello".to_owned()));

        reduce_view_event(
            &mut state,
            AgentViewEvent::Startup {
                model: "second-model".to_owned(),
                thinking_effort: Some("xhigh".to_owned()),
            },
        )
        .unwrap();

        assert_eq!(state.units.len(), 1);
        assert!(matches!(&state.units[0], RenderUnit::Prompt(prompt) if prompt == "hello"));
        assert_eq!(
            state.startup,
            Some(StartupInfo {
                model: "second-model".to_owned(),
                thinking_effort: Some("xhigh".to_owned()),
            })
        );
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
    fn finalizes_text_when_response_completes() {
        let mut units = Vec::new();
        let mut events = vec![
            AgentViewEvent::TextDelta("**hello**".to_owned()),
            AgentViewEvent::Completed,
        ];

        parse_units(&mut events, &mut units).unwrap();

        assert!(matches!(
            &units[..],
            [
                RenderUnit::ParsedMarkdown(blocks),
                RenderUnit::Separator,
            ] if matches!(
                &blocks[..],
                [MarkdownBlock::Paragraph(content)]
                    if matches!(&content[..], [markdown::Inline::Strong(_)])
            )
        ));
    }

    #[test]
    fn finalizes_all_text_segments_in_the_current_response() {
        let tool = presentation(ToolCallStatus::Running);
        let mut units = Vec::new();
        let mut events = vec![
            AgentViewEvent::TextDelta("before".to_owned()),
            AgentViewEvent::Tool(tool),
            AgentViewEvent::TextDelta("after".to_owned()),
            AgentViewEvent::Completed,
        ];

        parse_units(&mut events, &mut units).unwrap();

        assert!(matches!(
            &units[..],
            [
                RenderUnit::ParsedMarkdown(_),
                RenderUnit::Tool(_),
                RenderUnit::ParsedMarkdown(_),
                RenderUnit::Separator,
            ]
        ));
    }

    #[test]
    fn completion_does_not_reparse_previous_responses_or_create_empty_markdown() {
        let historical = vec![MarkdownBlock::Paragraph(vec![markdown::Inline::Text(
            "history".to_owned(),
        )])];
        let mut units = vec![
            RenderUnit::ParsedMarkdown(historical.clone()),
            RenderUnit::Separator,
            RenderUnit::Tool(presentation(ToolCallStatus::Running)),
        ];
        let mut events = vec![AgentViewEvent::Completed, AgentViewEvent::Completed];

        parse_units(&mut events, &mut units).unwrap();

        assert_eq!(units.len(), 4);
        assert!(matches!(
            &units[0],
            RenderUnit::ParsedMarkdown(blocks) if blocks == &historical
        ));
        assert!(matches!(&units[2], RenderUnit::Tool(_)));
        assert!(matches!(&units[3], RenderUnit::Separator));
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
