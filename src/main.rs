use std::sync::Arc;

use clap::Parser;
use iocraft::{ElementExt, element};
use tokio::sync::Mutex;

use crate::{
    agent::Agent,
    bridge::UiBridge,
    provider::openai::{OpenAIProvider, OpenAIProviderConfig},
    ui::UI,
};

mod agent;
mod bridge;
mod bus;
mod cli;
mod context;
mod event;
mod logger;
mod provider;
mod tool;
mod tui;
mod ui;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _logging_guard = logger::init(".h")?;
    tracing::info!(event = "app.starting");

    let args = cli::Args::parse();

    match cli::resolve_session(&args).await? {
        cli::Session::New => main_loop(None).await,
        cli::Session::Resume(id) => main_loop(Some(id)).await,
        cli::Session::Quit => {
            tracing::info!(event = "app.exited_without_session");
            Ok(())
        }
    }
}

/// Runs one session to completion. `id` names an archived session to pick up
/// where it left off; `None` starts a fresh one.
async fn main_loop(id: Option<String>) -> anyhow::Result<()> {
    let provider = OpenAIProvider::from_config(OpenAIProviderConfig::from_env()?);

    let (bridge, ui_request_rx) = UiBridge::new();

    let mut agent = Agent::new(provider);
    let bus_rx = agent.subscribe_view();
    agent.with_internal_tools(bridge)?;

    match &id {
        // A resumed context already carries the system messages the original
        // session was built with, so injecting them again would duplicate them.
        Some(id) => {
            agent.resume(id).await?;
        }
        None => {
            agent
                .with_global_prompts()
                .await?
                .with_workspace_info()
                .await?;
        }
    }

    agent.initialize()?;

    // After `initialize`, so the banner is on screen before the history under it.
    if id.is_some() {
        agent.rebroadcast_all_view();
    }

    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);

    tracing::info!(event = "app.ready");

    let worker = tokio::spawn(async move {
        while let Some(prompt) = rx.recv().await {
            match agent.continue_turn(prompt).await {
                Ok(_) => {}
                Err(e) => {
                    tracing::error!(
                        event = "agent.worker.failed",
                        operation = "continue_turn",
                        error_class = "agent_turn_error",
                        error = e.to_string(),
                    );
                }
            }
        }

        tracing::info!(event = "agent.worker.closed");

        // The loop above ends only once every prompt sender is gone, which means
        // the UI is down and this task is the last owner of the session. Archive
        // here rather than in `main_loop`, which no longer holds the agent.
        let archived = agent.archive().await;

        if let Err(e) = &archived {
            tracing::error!(
                event = "agent.archive.failed",
                operation = "archive",
                error_class = "archive_error",
                error = e.to_string(),
            );
        }

        archived
    });

    element!(UI(
        committer: Some(tx),
        event_rx: Arc::new(Mutex::new(Some(bus_rx))),
        ui_request_rx: Arc::new(Mutex::new(Some(ui_request_rx))),
    ))
    .render_loop()
    .fullscreen()
    .await?;

    tracing::info!(event = "app.ui.closed");

    // Quitting the UI dropped the last prompt sender, so the worker is either
    // done or finishing the turn it was in the middle of. Wait for it, both to
    // let that turn land in the archive and to surface a failed write.
    worker.await??;

    tracing::info!(event = "app.archived");
    Ok(())
}
