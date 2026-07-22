use std::sync::Arc;

use iocraft::{ElementExt, element};
use tokio::sync::Mutex;

use crate::{
    agent::{Agent, NextTurn},
    provider::openai::{OpenAIProvider, OpenAIProviderConfig},
    ui2::UI,
};

mod agent;
mod bus;
mod context;
mod event;
mod provider;
// mod ui;
mod marcos;
mod ui2;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let provider = OpenAIProvider::from_config(OpenAIProviderConfig::from_env()?);

    let mut agent = Agent::new(provider);

    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);

    let bus_rx = agent.subscribe();

    tokio::spawn(async move {
        while let Some(prompt) = rx.recv().await {
            if let Err(e) = agent.next_turn(NextTurn::Prompt(prompt)).await {
                panic!("Failed to do next turn {e}")
            };
        }

        anyhow::Ok(())
    });

    element!(UI(committer: Some(tx), event_rx: Arc::new(Mutex::new(Some(bus_rx)))))
        .render_loop()
        .await?;

    Ok(())
}
