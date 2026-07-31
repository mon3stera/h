use tokio::sync::mpsc::{UnboundedReceiver, error::TryRecvError};
use tokio_util::sync::CancellationToken;

use crate::{agent::Agent, event::AgentEvent, input::UserInput, provider::Provider};

/// Runs one complete agent turn and returns only its final text response.
///
/// The agent is consumed so this ephemeral lifecycle cannot accidentally fall
/// through to the interactive session's archive step.
pub async fn run<P>(mut agent: Agent<P>, prompt: impl Into<UserInput>) -> anyhow::Result<String>
where
    P: Provider,
{
    let mut events = agent.subscribe();
    agent.initialize()?;

    agent
        .continue_turn(prompt.into(), CancellationToken::new())
        .await?;

    Ok(final_response(&mut events))
}

fn final_response(events: &mut UnboundedReceiver<AgentEvent>) -> String {
    let (mut current, mut final_text) = (String::new(), String::new());

    loop {
        match events.try_recv() {
            Ok(AgentEvent::TextDelta(delta)) => current.push_str(&delta),
            Ok(AgentEvent::Completed) => final_text = std::mem::take(&mut current),
            Ok(AgentEvent::Reasoning)
            | Ok(AgentEvent::Search(_))
            | Ok(AgentEvent::ToolCallStarted(_))
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
        fs,
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use futures::stream;

    use super::*;
    use crate::{
        context::Message,
        event::{CompletedReason, ProviderSignal},
        tool::ToolDefinition,
    };

    struct TwoRoundProvider {
        requests: AtomicUsize,
    }

    struct TempArchive {
        path: PathBuf,
    }

    impl TempArchive {
        fn new() -> Self {
            Self {
                path: std::env::temp_dir()
                    .join(format!("h-headless-archive-{}", uuid::Uuid::new_v4())),
            }
        }
    }

    impl Drop for TempArchive {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[async_trait::async_trait]
    impl Provider for TwoRoundProvider {
        fn model(&self) -> &str {
            "test-model"
        }

        fn thinking_effort(&self) -> Option<&str> {
            None
        }

        fn define_tools(&mut self, _specs: Vec<ToolDefinition>) -> anyhow::Result<()> {
            Ok(())
        }

        async fn stream(
            &self,
            _input: &[Message],
        ) -> anyhow::Result<crate::provider::ProviderEventStream> {
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
        let agent = Agent::new(provider);

        let response = run(agent, "What tools can you use?".to_owned())
            .await
            .unwrap();

        assert_eq!(response, "The available tools are ...");
    }

    #[tokio::test]
    async fn a_headless_turn_does_not_create_an_archive() {
        let archive = TempArchive::new();
        let provider = TwoRoundProvider {
            requests: AtomicUsize::new(0),
        };
        let mut agent = Agent::new(provider);
        agent.with_archive_dir(&archive.path);

        run(agent, "What tools can you use?".to_owned())
            .await
            .unwrap();

        assert!(!archive.path.exists());
    }
}
