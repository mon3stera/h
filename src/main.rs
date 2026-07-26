use std::sync::Arc;

use iocraft::{ElementExt, element};
use tokio::sync::Mutex;

use crate::{
    agent::Agent,
    provider::openai::{OpenAIProvider, OpenAIProviderConfig},
    ui::UI,
};

mod agent;
mod bus;
mod context;
mod event;
mod logger;
mod provider;
mod tool;
mod ui;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _logging_guard = logger::init(".h")?;
    tracing::info!(event = "app.starting");

    let provider = OpenAIProvider::from_config(OpenAIProviderConfig::from_env()?);

    let mut agent = Agent::new(provider);
    let bus_rx = agent.subscribe_view();
    agent
        .with_internal_tools()?
        .with_global_prompts()
        .await?
        .with_workspace_info()
        .await?
        .initialize()?;

    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);

    tracing::info!(event = "app.ready");

    tokio::spawn(async move {
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
        anyhow::Ok(())
    });

    element!(UI(committer: Some(tx), event_rx: Arc::new(Mutex::new(Some(bus_rx)))))
        .render_loop()
        .fullscreen()
        .await?;

    tracing::info!(event = "app.ui.closed");
    Ok(())
}
