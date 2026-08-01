use std::io::Write;

use clap::Parser;
use h_core::{
    agent::Agent,
    event::AgentCommand,
    interaction::Bridge,
    provider::{
        anthropic::{AnthropicProvider, AnthropicProviderConfig},
        openai::{OpenAIProvider, OpenAIProviderConfig},
    },
};
use h_memory::{
    Store as MemoryStore,
    tool::{ReadPresenter, ReadTool, SearchPresenter, SearchTool, WritePresenter, WriteTool},
};
use tokio::sync::mpsc;

mod bootstrap;
mod cli;
mod config;
mod logger;
mod provider;
mod serve;

use bootstrap::Bootstrap;
use config::{Config, ProfileConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _logging_guard = logger::init(".h")?;
    tracing::info!(event = "app.starting");

    let mut args = cli::Args::parse();

    if let Some(cli::Command::Serve(serve_args)) = args.command.take() {
        return serve::run(serve_args).await;
    }

    let (prompt, bootstrap) = (args.prompt.take(), Bootstrap::from(args.instruction.take()));

    if let Some(prompt) = prompt {
        return run_prompt(prompt, args.profile.as_deref(), bootstrap).await;
    }

    let profile = args.profile.as_deref();

    match cli::resolve_session(&args).await? {
        cli::Session::New => main_loop(None, profile, bootstrap).await,
        cli::Session::Resume(id) => main_loop(Some(id), profile, Bootstrap::Default).await,
        cli::Session::Quit => {
            tracing::info!(event = "app.exited_without_session");
            Ok(())
        }
    }
}

/// Runs one session to completion. `id` names an archived session to pick up
/// where it left off; `None` starts a fresh one. `profile` is the `--profile`
/// override, ignored when resuming because `--profile` and `--resume` conflict.
async fn main_loop(
    id: Option<String>,
    profile: Option<&str>,
    bootstrap: Bootstrap,
) -> anyhow::Result<()> {
    let (bridge, interaction_rx) = Bridge::new();
    let (mut agent, context_window, mcp) =
        build_agent(id.as_deref(), profile, bootstrap, bridge).await?;
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

    let worker = tokio::spawn(async move {
        agent.run(command_rx).await;

        let archived = agent.archive().await;
        if let Err(error) = &archived {
            tracing::error!(
                event = "agent.archive.failed",
                operation = "archive",
                error_class = "archive_error",
                error = error.to_string(),
            );
        }

        archived
    });

    let ui = h_tui::app::run(commands, bus_rx, interaction_rx, history, context_window).await;

    tracing::info!(event = "app.ui.closed");

    // Quitting the UI dropped the last command sender, so the worker is either
    // done or finishing the turn it was in the middle of. Wait for it, both to
    // let that turn land in the archive and to surface a failed write.
    let worker = worker.await;
    let mcp = mcp.close().await;

    ui?;
    worker??;
    mcp?;

    tracing::info!(event = "app.archived");
    Ok(())
}

async fn run_prompt(
    prompt: String,
    profile: Option<&str>,
    bootstrap: Bootstrap,
) -> anyhow::Result<()> {
    let (bridge, interaction_rx) = Bridge::new();
    drop(interaction_rx);

    let (agent, _, mcp) = build_agent(None, profile, bootstrap, bridge).await?;

    tracing::info!(event = "app.ready", mode = "headless");

    let response = h_core::headless::run(agent, prompt).await;
    let closed = mcp.close().await;
    let response = response?;
    closed?;

    let mut stdout = std::io::stdout().lock();
    stdout.write_all(response.as_bytes())?;
    if !response.is_empty() && !response.ends_with('\n') {
        stdout.write_all(b"\n")?;
    }
    stdout.flush()?;

    tracing::info!(event = "app.headless.completed");
    Ok(())
}

/// Builds a session up to, but not including, provider initialization. Callers
/// can subscribe to the event stream they need before initialization emits its
/// first events.
pub(crate) async fn build_agent(
    id: Option<&str>,
    profile: Option<&str>,
    bootstrap: Bootstrap,
    bridge: Bridge,
) -> anyhow::Result<(Agent<provider::Client>, usize, h_mcp::Runtime)> {
    let mut config = Config::load().await?;
    config.select(profile)?;
    let (provider, provider_name) = match config.profile() {
        ProfileConfig::OpenAI(openai) => (
            provider::Client::OpenAI(OpenAIProvider::from_config(OpenAIProviderConfig::new(
                openai.base_url(),
                openai.bearer_token(),
                config.model(),
                config.reasoning_effort(),
            ))),
            openai.name().to_owned(),
        ),
        ProfileConfig::Anthropic(anthropic) => (
            provider::Client::Anthropic(AnthropicProvider::from_config(
                AnthropicProviderConfig::new(
                    anthropic.base_url(),
                    anthropic.api_key().map(str::to_owned),
                    anthropic.auth_token().map(str::to_owned),
                    config.model(),
                    config.reasoning_effort(),
                ),
            )?),
            anthropic.name().to_owned(),
        ),
    };
    let (context_window, auto_compact_token_limit, tool_summary_turn_interval, mcp_config) = (
        config.context_window(),
        config.auto_compact_token_limit(),
        config.tool_summary_turn_interval(),
        config.mcp().clone(),
    );

    tracing::info!(
        event = "config.loaded",
        profile_id = config.profile_id(),
        profile_name = provider_name,
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

    let memory = MemoryStore::discover().await?;
    agent
        .register_tool_with_presenter(ReadTool::new(memory.clone()), ReadPresenter)
        .register_tool_with_presenter(SearchTool::new(memory.clone()), SearchPresenter)
        .register_tool_with_presenter(WriteTool::new(memory.clone()), WritePresenter);

    match (id, bootstrap) {
        // A resumed context already carries the system messages the original
        // session was built with, so injecting them again would duplicate them.
        (Some(id), Bootstrap::Default) => {
            agent.resume(id).await?;
        }
        (Some(_), Bootstrap::Instruction(_)) => {
            anyhow::bail!("an instruction bootstrap cannot resume an archived session")
        }
        (None, Bootstrap::Default) => {
            let memory = memory.snapshot().await?;
            let executable = std::env::current_exe()?;

            agent
                .with_harness_prompt(&executable)
                .with_global_prompts()
                .await?
                .with_skills()
                .await?;
            agent
                .with_system_prompt(memory.content)
                .with_workspace_info()
                .await?;
        }
        (None, Bootstrap::Instruction(instruction)) => {
            agent.with_system_prompt(instruction);
        }
    }

    let mcp = h_mcp::Runtime::start(&mcp_config).await?;
    let mcp_tool_count = match mcp.register(&mut agent) {
        Ok(count) => count,
        Err(error) => {
            if let Err(close_error) = mcp.close().await {
                tracing::warn!(
                    event = "mcp.runtime.close.failed",
                    error = close_error.to_string(),
                );
            }

            return Err(error);
        }
    };

    tracing::info!(
        event = "mcp.runtime.started",
        server_count = mcp.server_count(),
        tool_count = mcp_tool_count,
    );

    Ok((agent, context_window, mcp))
}
