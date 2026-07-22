use std::sync::Arc;

use iocraft::prelude::*;
use tokio::sync::{
    Mutex,
    mpsc::{Sender, UnboundedReceiver},
};

use crate::{event::AgentEvent, marcos::log_error};

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

#[derive(Debug, Clone)]
enum RenderUnit {
    Text(String),
    Prompt(String),
    Separator,
}

#[derive(Debug, Props, Default)]
pub struct UIProp {
    pub committer: Option<Sender<String>>,
    pub event_rx: Arc<Mutex<Option<UnboundedReceiver<AgentEvent>>>>,
}

#[component]
pub fn UI(mut hooks: Hooks, props: &UIProp) -> impl Into<AnyElement<'static>> {
    let mut units = hooks.use_state(|| Vec::<RenderUnit>::new());

    let event_rx = props.event_rx.clone();
    hooks.use_future(async move {
        let mut rx = event_rx.lock().await.take().unwrap();

        while let Some(event) = rx.recv().await {
            let mut inner = units.write();
            log_error!(parse_units(&mut vec![event], &mut inner));
        }
    });

    let committer = props.committer.clone();
    let input_handler = hooks.use_async_handler(move |s: String| {
        let committer = committer.clone().unwrap();

        Box::pin(async move {
            units.write().push(RenderUnit::Prompt(s.clone()));
            log_error!(committer.send(s).await);
        })
    });

    let (width, height) = hooks.use_terminal_size();

    element! {
        View(width: width, height: height, flex_direction: FlexDirection::Column) {
            View(width: 100pct, flex_grow: 1.0_f32, overflow: Overflow::Hidden) {
                DisplayArea(units: units.read().iter().cloned().collect::<Vec<_>>())
            }

            Textarea(on_submit: input_handler)
        }
    }
}

#[derive(Debug, Default, Props)]
struct DisplayAreaProp {
    units: Vec<RenderUnit>,
}

#[derive(Props, Default)]
struct TextareaProp {
    on_submit: Handler<String>,
}

#[component]
fn DisplayArea<'a>(mut hooks: Hooks, props: &DisplayAreaProp) -> impl Into<AnyElement<'a>> {
    let (width, _) = hooks.use_terminal_size();

    element! {
        View(width: 100pct, flex_direction: FlexDirection::Column, row_gap: 1) {
            #(props.units.iter().map(|unit| {
                match unit {
                    RenderUnit::Text(text) => element! { Text(content: format!("{}", text.as_str()), color: Some(Color::Cyan)) },
                    RenderUnit::Prompt(text) => element! { Text(content: format!("❯ {}", text.as_str()), color: Some(Color::Yellow), italic: true) },
                    RenderUnit::Separator => element! { Text(content: "─".repeat(width as usize).to_string()) },
                }
            }))
        }
    }
}

#[component]
fn Textarea<'a>(mut hooks: Hooks, props: &TextareaProp) -> impl Into<AnyElement<'a>> {
    let mut input = hooks.use_state(|| "".to_string());

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
        View(width: 100pct, min_height: 3, border_style: BorderStyle::Round, border_edges: Some(Edges::Top | Edges::Bottom)) {
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
