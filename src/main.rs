use std::{collections::VecDeque, io::Write};

use clap::Parser;
use tokio::sync::mpsc::{self, Receiver, error::TryRecvError};
use tokio_util::sync::CancellationToken;

use crate::{
    agent::Agent,
    bridge::UiBridge,
    config::{Config, ProviderConfig},
    event::AgentCommand,
    provider::{
        Provider,
        openai::{OpenAIProvider, OpenAIProviderConfig},
    },
};

mod agent;
mod bridge;
mod bus;
mod cli;
mod command;
mod config;
mod context;
mod event;
mod headless;
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

    if let Some(prompt) = args.prompt {
        return run_prompt(prompt).await;
    }

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
    let (bridge, ui_request_rx) = UiBridge::new();
    let (mut agent, context_window) = build_agent(id.as_deref(), bridge).await?;
    let bus_rx = agent.subscribe_view();

    agent.initialize()?;

    // After `initialize`, so the banner is on screen before the history under it.
    if id.is_some() {
        agent.rebroadcast_all_view();
    }

    // Taken before the agent moves into the worker: a resumed session's prompts
    // seed the prompt box so they can be recalled.
    let history = agent.prompts();

    let (commands, command_rx) = mpsc::channel::<AgentCommand>(8);

    tracing::info!(event = "app.ready", mode = "interactive");

    let worker = tokio::spawn(run_agent(agent, command_rx));

    tui::app::run(commands, bus_rx, ui_request_rx, history, context_window).await?;

    tracing::info!(event = "app.ui.closed");

    // Quitting the UI dropped the last command sender, so the worker is either
    // done or finishing the turn it was in the middle of. Wait for it, both to
    // let that turn land in the archive and to surface a failed write.
    worker.await??;

    tracing::info!(event = "app.archived");
    Ok(())
}

async fn run_prompt(prompt: String) -> anyhow::Result<()> {
    let (bridge, ui_request_rx) = UiBridge::new();
    drop(ui_request_rx);

    let (mut agent, _) = build_agent(None, bridge).await?;
    agent.initialize()?;

    tracing::info!(event = "app.ready", mode = "headless");

    let result = headless::run(&mut agent, prompt).await;
    let archived = agent.archive().await;
    let response = result?;

    let mut stdout = std::io::stdout().lock();
    stdout.write_all(response.as_bytes())?;
    if !response.is_empty() && !response.ends_with('\n') {
        stdout.write_all(b"\n")?;
    }
    stdout.flush()?;

    archived?;

    tracing::info!(event = "app.archived");
    Ok(())
}

/// Builds a session up to, but not including, provider initialization. Callers
/// can subscribe to the event stream they need before initialization emits its
/// first events.
async fn build_agent(
    id: Option<&str>,
    bridge: UiBridge,
) -> anyhow::Result<(Agent<OpenAIProvider>, usize)> {
    let config = Config::load().await?;
    let ProviderConfig::OpenAI(openai) = config.provider();

    let provider = OpenAIProvider::from_config(OpenAIProviderConfig::new(
        openai.base_url(),
        openai.bearer_token(),
        config.model(),
        config.reasoning_effort(),
    ));
    let (context_window, auto_compact_token_limit, tool_summary_turn_interval) = (
        config.context_window(),
        config.auto_compact_token_limit(),
        config.tool_summary_turn_interval(),
    );

    tracing::info!(
        event = "config.loaded",
        provider_id = config.provider_id(),
        provider_name = openai.name(),
        model = config.model(),
        context_window,
        auto_compact_token_limit,
        tool_summary_turn_interval = tool_summary_turn_interval.get(),
    );

    // The provider owns its copied credential now; do not keep a second copy
    // in the parsed configuration for the lifetime of the session.
    drop(config);

    let mut agent = Agent::new(provider);
    agent
        .with_auto_compact_token_limit(auto_compact_token_limit)
        .with_tool_summary_turn_interval(tool_summary_turn_interval);
    agent.with_internal_tools(bridge)?;

    match id {
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

    Ok((agent, context_window))
}

/// Owns the mutable agent while still listening for UI control commands during
/// a turn. Prompts and slash commands retain their order, while `Cancel` always
/// targets the turn currently being polled.
async fn run_agent<P>(
    mut agent: Agent<P>,
    mut commands: Receiver<AgentCommand>,
) -> anyhow::Result<()>
where
    P: Provider,
{
    let (mut queued, mut accepting) = (VecDeque::new(), true);

    loop {
        let command = loop {
            if !accepting {
                break None;
            }

            if let Some(command) = queued.pop_front() {
                break Some(command);
            }

            match commands.recv().await {
                Some(AgentCommand::Cancel) => {}
                Some(command) => break Some(command),
                None => {
                    accepting = false;
                    break None;
                }
            }
        };
        let Some(command) = command else {
            break;
        };

        match command {
            AgentCommand::Prompt(prompt) => {
                let cancellation = CancellationToken::new();
                let result = {
                    let turn = agent.continue_turn(prompt, cancellation.clone());
                    tokio::pin!(turn);

                    loop {
                        tokio::select! {
                            result = &mut turn => break result,
                            command = commands.recv(), if accepting => match command {
                                Some(AgentCommand::Cancel) => cancellation.cancel(),
                                Some(command) => queued.push_back(command),
                                None => {
                                    accepting = false;
                                    queued.clear();
                                    cancellation.cancel();
                                }
                            }
                        }
                    }
                };

                if let Err(e) = result {
                    tracing::error!(
                        event = "agent.worker.failed",
                        operation = "continue_turn",
                        error_class = "agent_turn_error",
                        error = e.to_string(),
                    );
                }
            }
            AgentCommand::Run(command) => {
                let _ = agent.run_command(command).await;
            }
            AgentCommand::Cancel => {}
        }

        // If the turn and a queued Esc become ready together, the turn has
        // already ended. Discard that stale cancellation before starting the
        // next queued prompt.
        loop {
            match commands.try_recv() {
                Ok(AgentCommand::Cancel) => {}
                Ok(command) => queued.push_back(command),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    accepting = false;
                    queued.clear();
                    break;
                }
            }
        }
    }

    tracing::info!(event = "agent.worker.closed");

    // The UI is down and this task is the last owner of the session. Archive
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
}
