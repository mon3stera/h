use crate::{
    bus::EventBus,
    event::AgentEvent,
    provider::{Provider, TurnStart},
};
use futures::StreamExt;
use tokio::sync::mpsc::UnboundedReceiver;

pub enum NextTurn {
    Prompt(String),
    Continue,
}

pub struct Agent<P> {
    bus: EventBus<AgentEvent>,
    provider: P,
}

impl<P> Agent<P>
where
    P: Provider,
{
    pub fn new(provider: P) -> Self {
        Self {
            bus: EventBus::new(),
            provider,
        }
    }

    pub fn subscribe(&self) -> UnboundedReceiver<AgentEvent> {
        self.bus.subscribe()
    }

    pub async fn next_turn(&mut self, turn: NextTurn) -> anyhow::Result<()> {
        match turn {
            NextTurn::Prompt(prompt) => {
                let mut stream = self
                    .provider
                    .stream(TurnStart::UserMessage(prompt.into()))
                    .await?;

                loop {
                    match stream.next().await {
                        Some(Ok(event)) => {
                            let signal = self.provider.handle(event).await?;

                            let agent_event: AgentEvent = signal.into();

                            self.bus.broadcast(agent_event);
                        }
                        Some(e) => {
                            e?;
                        }
                        None => break Ok(()),
                    }
                }
            }
            NextTurn::Continue => todo!(),
        }
    }
}
