use std::vec;

use crossterm::event::{self, Event, KeyCode};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Paragraph},
};
use ratatui_textarea::{TextArea, WrapMode};
use tokio::sync::mpsc::UnboundedReceiver;
use unicode_width::UnicodeWidthStr;

use crate::event::AgentEvent;

pub fn render_ui(frame: &mut Frame<'_>, textarea: &TextArea) -> anyhow::Result<()> {
    let area = frame.area();

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![
            Constraint::Min(1),
            Constraint::Length(textarea_height(textarea, area.width)),
        ])
        .split(frame.area());

    frame.render_widget(
        Paragraph::new("Dialogue").block(Block::new().borders(Borders::ALL)),
        layout[0],
    );

    render_textarea(frame, layout[1], textarea);

    Ok(())
}

fn collect_text(events: &[AgentEvent]) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    
    let mut buf = String::new();

    for event in events {
        match event {
            AgentEvent::TextDelta(delta) => buf = format!("{}{}", buf, delta),
            x => {
                if !buf.is_empty() {
                    events.push()
                }
            }
        }
    }

    events
}

fn build_constraints(events: &[AgentEvent]) -> impl Iterator<Item = Constraint> {
    events
        .iter()
        .map(|e| match e {
            Agent
        })
}

fn render_text(frame: &mut Frame<'_>, layout: Rect) {

}

fn render_events(frame: &mut Frame<'_>, layout: Rect, events: &[AgentEvent]) {
    let sub_layouts = Layout::default()
        .direction(Direction::Vertical)
        .constraints()
}

fn render_textarea(frame: &mut Frame<'_>, layout: Rect, textarea: &TextArea) { 
    let block = Block::new()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_style(Style::default().fg(Color::DarkGray));

    let inner_area = block.inner(layout);

    frame.render_widget(
        block,
        layout,
    );

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

pub fn run_ui() -> anyhow::Result<()> {
    ratatui::run(|mut terminal| -> anyhow::Result<()> {
        let mut textarea = textarea();

        loop {
            terminal.draw(|frame| render_ui(frame, &mut textarea).unwrap())?;

            if let Event::Key(key) = crossterm::event::read()? {
                if key.code == KeyCode::Esc {
                    break Ok(());
                }

                textarea.input(key);
            }
        }
    })?;

    Ok(())
}
