use std::{any, time::Duration, vec};

use crossterm::event::{self, Event, KeyCode};
use ratatui::{
    Frame, layout::{Constraint, Direction, Layout, Rect}, style::{Color, Style, Styled}, widgets::{Block, Borders, List, Paragraph, Wrap},
};
use ratatui_textarea::{TextArea, WrapMode};
use tokio::{
    sync::mpsc::{Sender, UnboundedReceiver, error::TryRecvError},
    task::JoinHandle,
};
use unicode_width::UnicodeWidthStr;

use crate::event::AgentEvent;

enum RenderUnit {
    Text(String),
    Separator,
    Prompt(String),
}

impl TryFrom<AgentEvent> for RenderUnit {
    type Error = anyhow::Error;

    fn try_from(value: AgentEvent) -> Result<Self, Self::Error> {
        match value {
            AgentEvent::TextDelta(_) => anyhow::bail!("must merge text delta"),
            AgentEvent::Completed => Ok(RenderUnit::Separator),
            _ => anyhow::bail!("cannot convert to RenderUnit"),
        }
    }
}

impl TryFrom<&AgentEvent> for RenderUnit {
    type Error = anyhow::Error;

    fn try_from(value: &AgentEvent) -> Result<Self, Self::Error> {
        value.clone().try_into()
    }
}

fn is_need_rendered_event(event: &AgentEvent) -> bool {
    matches!(event, AgentEvent::TextDelta(_) | AgentEvent::Completed)
}

fn render_ui(
    frame: &mut Frame<'_>,
    textarea: &TextArea,
    units: &[RenderUnit],
) -> anyhow::Result<()> {
    let area = frame.area();

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![
            Constraint::Min(1),
            Constraint::Length(textarea_height(textarea, area.width)),
        ])
        .split(frame.area());

    render_units(frame, layout[0], units);
    render_textarea(frame, layout[1], textarea);

    Ok(())
}

fn preprocess_events(events: &[AgentEvent]) -> anyhow::Result<Vec<RenderUnit>> {
    let mut units = Vec::new();

    let mut buf = String::new();

    for event in events {
        match event {
            AgentEvent::TextDelta(delta) => buf = format!("{}{}", buf, delta),
            _ if !is_need_rendered_event(event) => {}
            x => {
                if !buf.is_empty() {
                    units.push(RenderUnit::Text(buf.clone()));
                }

                units.push(x.try_into()?);

                buf = String::new();
            }
        }
    }

    if !buf.is_empty() {
        units.push(RenderUnit::Text(buf));
    }

    Ok(units)
}

fn text_height(width: u16, text: impl AsRef<str>) -> u16 { 
    text
        .as_ref()
        .lines() 
        .map(|line| {
            let line_width = UnicodeWidthStr::width(line) as u16;
            line_width.max(1).div_ceil(width)
        })
        .sum()
} 

fn wrap_str(text: impl AsRef<str>, width: u16) -> Vec<String> {
    textwrap::wrap(text.as_ref(), width as usize)
        .into_iter()
        .map(|c| c.into_owned())
        .collect()
}

fn text_unit_constraint(text: impl AsRef<str>, width: u16) -> Constraint {
    Constraint::Length(text_height(width, text))
}

fn build_constraints(units: &[RenderUnit], width: u16) -> impl Iterator<Item = Constraint> {
    units.iter().map(move |e| match e {
        RenderUnit::Text(text) => text_unit_constraint(text, width),
        RenderUnit::Separator => Constraint::Length(1),
        RenderUnit::Prompt(_) => Constraint::Length(3),
        _ => todo!(),
    })
}

fn render_text(frame: &mut Frame<'_>, layout: Rect, text: impl Into<String>) {
    frame.render_widget(
        Paragraph::new(text.into()).set_style(Style::default().cyan()).wrap(Wrap { trim: false }),
        layout,
    );
}

fn render_prompt(frame: &mut Frame<'_>, layout: Rect, text: impl Into<String>) {
    let text = format!("❯ {}", text.into());

    frame.render_widget(
        Paragraph::new(text)
            .set_style(Style::default().yellow())
            .wrap(Wrap { trim: false })
            .block(Block::new().borders(Borders::ALL)),
        layout,
    );
}

fn render_sep(frame: &mut Frame<'_>, layout: Rect) {
    let sep = "─".repeat(frame.area().width as usize);

    frame.render_widget(
        Paragraph::new(sep).set_style(Style::default().cyan()),
        layout,
    );
}

fn render_unit(frame: &mut Frame<'_>, layout: Rect, unit: &RenderUnit) {
    match unit {
        RenderUnit::Text(text) => render_text(frame, layout, text),
        RenderUnit::Separator => render_sep(frame, layout),
        RenderUnit::Prompt(text) => render_prompt(frame, layout, text),
        _ => {}
    }
}

fn build_list(units: &[RenderUnit], width: u16) -> List {
    let items = Vec::new();

    

    List::new(items);
}

fn render_units(frame: &mut Frame<'_>, layout: Rect, units: &[RenderUnit]) {
    let constrains = build_constraints(&units, frame.area().width);

    let list_units = Vec::new();

    let list = List::new(list_units);

    frame.render_stateful_widget(list, layout, state);

    let sub_layouts = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constrains)
        .split(layout);

    for (unit, sub_layout) in units.iter().zip(sub_layouts.iter()) {
        render_unit(frame, *sub_layout, unit);
    }
}

fn render_textarea(frame: &mut Frame<'_>, layout: Rect, textarea: &TextArea) {
    let block = Block::new()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner_area = block.inner(layout);

    frame.render_widget(block, layout);

    let [prompt_area, textarea_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(vec![Constraint::Length(2), Constraint::Min(1)])
        .areas(inner_area);

    frame.render_widget(
        Paragraph::new("❯").style(Style::default().fg(Color::Cyan)),
        prompt_area,
    );
    frame.render_widget(textarea, textarea_area);
}

fn textarea<'a>() -> TextArea<'a> {
    let mut textarea = TextArea::default();

    textarea.set_cursor_line_style(Style::default());
    textarea.set_tab_length(4);

    textarea.set_placeholder_text("Welcome to h!");

    textarea.set_wrap_mode(WrapMode::WordOrGlyph);

    textarea
}

fn textarea_height(textarea: &TextArea, width: u16) -> u16 {
    const MAX_HEIGHT: u16 = 10;
    const MIN_HEIGHT: u16 = 3;
    const BORDER_HEIGHT: u16 = 2;

    let inner_width = usize::from(width).max(1);

    let visual_lines = textarea
        .lines()
        .iter()
        .map(|line| {
            let width = UnicodeWidthStr::width(line.as_str());
            width.saturating_add(2).max(1).div_ceil(inner_width)
        })
        .sum::<usize>();

    u16::try_from(visual_lines)
        .unwrap_or(u16::MAX)
        .saturating_add(BORDER_HEIGHT)
        .clamp(MIN_HEIGHT, MAX_HEIGHT)
}

fn fetch_events(
    rx: &mut UnboundedReceiver<AgentEvent>,
    events: &mut Vec<AgentEvent>,
) -> anyhow::Result<()> {
    loop {
        match rx.try_recv() {
            Ok(event) => events.push(event),
            Err(TryRecvError::Empty) => break Ok(()),
            x @ Err(_) => {
                x?;
            }
        }
    }
}

fn parse_units(events: &mut Vec<AgentEvent>, units: &mut Vec<RenderUnit>) -> anyhow::Result<()> {
    for unit in preprocess_events(events)? {
        match (units.last_mut(), &unit) {
            (Some(RenderUnit::Text(t)), RenderUnit::Text(nt)) => t.push_str(nt.as_str()),
            _ => {
                units.push(unit);
            }
        }
    }

    events.clear();
    Ok(())
}

pub fn run_ui(
    mut rx: UnboundedReceiver<AgentEvent>,
    committer: Sender<String>,
) -> JoinHandle<anyhow::Result<()>> {
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let mut events = Vec::new();

        let mut units = Vec::new();

        ratatui::run(|terminal| -> anyhow::Result<()> {
            let mut textarea = textarea();

            loop {
                terminal.draw(|frame| render_ui(frame, &mut textarea, &units).unwrap())?;

                fetch_events(&mut rx, &mut events)?;

                parse_units(&mut events, &mut units)?;

                if event::poll(Duration::from_millis(10))? {
                    if let Event::Key(key) = crossterm::event::read()? {
                        match key.code {
                            KeyCode::Esc => {
                                break Ok(());
                            }
                            KeyCode::Enter => {
                                if !textarea.is_empty() {
                                    let prompt = textarea
                                        .lines()
                                        .iter()
                                        .filter(|e| !e.is_empty())
                                        .map(|e| e.to_string())
                                        .collect::<Vec<String>>()
                                        .join("");

                                    committer.blocking_send(prompt.clone())?;

                                    textarea.clear();

                                    units.push(RenderUnit::Prompt(prompt))
                                }
                            }
                            _ => {
                                textarea.input(key);
                            }
                        }
                    }
                }
            }
        })?;

        Ok(())
    })
}
