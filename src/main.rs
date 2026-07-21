use std::io::{Read, stdout};

use anyhow::Context;
use async_openai::{traits::EventType, types::responses::ResponseStreamEvent};
use futures::StreamExt;
use std::io::Write;

use crate::{
    agent::Agent,
    ui::{render_ui, run_ui},
};

use crossterm::event as crossterm_event;

mod agent;
mod bus;
mod context;
mod event;
mod provider;
mod ui;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    run_ui()
}
