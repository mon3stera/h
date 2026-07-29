use tokio::sync::mpsc::{UnboundedReceiver, error::TryRecvError};
use tokio_util::sync::CancellationToken;

use crate::{agent::Agent, event::AgentEvent, provider::Provider};

/// Runs one complete agent turn and returns only its final text response.
pub async fn run<P>(agent: &mut Agent<P>, prompt: String) -> anyhow::Result<String>
where
    P: Provider,
{
    let mut events = agent.subscribe();

    agent
        .continue_turn(prompt, CancellationToken::new())
        .await?;

    Ok(final_response(&mut events))
}

fn final_response(events: &mut UnboundedReceiver<AgentEvent>) -> String {
    let (mut current, mut final_text) = (String::new(), String::new());

    loop {
        match events.try_recv() {
            Ok(AgentEvent::TextDelta(delta)) => current.push_str(&delta),
            Ok(AgentEvent::Completed) => final_text = std::mem::take(&mut current),
            Ok(AgentEvent::ToolCallStarted(_))
            | Ok(AgentEvent::ToolCallCompleted(_))
            | Ok(AgentEvent::Unsupported) => {}
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        }
    }

    final_text
}

#[cfg(test)]
mod tests {
    use std::{
        pin::Pin,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use futures::{Stream, stream};

    use super::*;
    use crate::{
        context::Message,
        event::{CompletedReason, ProviderSignal},
        tool::ToolDefinition,
    };

    struct TwoRoundProvider {
        requests: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl Provider for TwoRoundProvider {
        type StreamEvent = ProviderSignal;

        fn model(&self) -> &str {
            "test-model"
        }

        fn thinking_effort(&self) -> Option<&str> {
            None
        }

        fn define_tools(&mut self, _specs: Vec<ToolDefinition>) -> anyhow::Result<()> {
            Ok(())
        }

        async fn handle(&mut self, event: Self::StreamEvent) -> anyhow::Result<ProviderSignal> {
            Ok(event)
        }

        async fn stream(
            &self,
            _input: &[Message],
        ) -> anyhow::Result<Pin<Box<dyn Stream<Item = anyhow::Result<Self::StreamEvent>> + Send>>>
        {
            let request = self.requests.fetch_add(1, Ordering::SeqCst);
            let signals = if request == 0 {
                vec![
                    ProviderSignal::TextDelta("I will inspect the tools.".to_owned()),
                    ProviderSignal::Completed {
                        reason: CompletedReason::NeedCall,
                    },
                ]
            } else {
                vec![
                    ProviderSignal::TextDelta("The available ".to_owned()),
                    ProviderSignal::TextDelta("tools are ...".to_owned()),
                    ProviderSignal::Completed {
                        reason: CompletedReason::Final,
                    },
                ]
            };

            Ok(Box::pin(stream::iter(signals.into_iter().map(Ok))))
        }
    }

    #[tokio::test]
    async fn only_the_last_provider_response_is_returned() {
        let provider = TwoRoundProvider {
            requests: AtomicUsize::new(0),
        };
        let mut agent = Agent::new(provider);
        agent.initialize().unwrap();

        let response = run(&mut agent, "What tools can you use?".to_owned())
            .await
            .unwrap();

        assert_eq!(response, "The available tools are ...");
    }
}
