use crate::{bus::EventBus, context::Context, event::AgentEvent, provider::Provider};
use anyhow::Context as AnyhowContext;
use futures::{Stream, StreamExt};
use tokio::sync::{
    broadcast::{self, Receiver, Sender},
    mpsc::UnboundedReceiver,
};

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
}
