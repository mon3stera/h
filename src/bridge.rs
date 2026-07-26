use tokio::sync::{mpsc, oneshot};

use crate::event::{AskAnswer, AskQuestion, UiRequest};

/// Requests queue here while the user works through them, so a burst never
/// blocks the agent before the UI has drained it.
const REQUEST_CAPACITY: usize = 8;

/// The agent-side half of the UI round trip.
///
/// Unlike [`EventBus`](crate::bus::EventBus), which broadcasts clonable events
/// one way, this carries a reply channel per request and is point-to-point.
/// Hand a clone to anything that needs an answer from the user; hand the
/// receiver to the UI.
#[derive(Clone)]
pub struct UiBridge {
    tx: mpsc::Sender<UiRequest>,
}

impl UiBridge {
    pub fn new() -> (Self, mpsc::Receiver<UiRequest>) {
        let (tx, rx) = mpsc::channel(REQUEST_CAPACITY);
        (Self { tx }, rx)
    }

    /// Puts a question to the user and waits for their answer.
    ///
    /// There is deliberately no timeout: an unanswered question blocks the
    /// caller until the user answers or the UI goes away.
    pub async fn ask(&self, question: AskQuestion) -> anyhow::Result<AskAnswer> {
        let (reply, answer) = oneshot::channel();

        if self
            .tx
            .send(UiRequest::Ask { question, reply })
            .await
            .is_err()
        {
            tracing::warn!(
                event = "ui_bridge.request.failed",
                operation = "send",
                error_class = "ui_unavailable"
            );
            anyhow::bail!("the user interface is no longer running, so it cannot be asked");
        }

        answer.await.map_err(|_| {
            tracing::warn!(
                event = "ui_bridge.request.failed",
                operation = "await_reply",
                error_class = "reply_dropped"
            );
            anyhow::anyhow!("the question was dismissed without an answer")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::UiBridge;
    use crate::event::{AskAnswer, AskQuestion, UiRequest};

    fn question() -> AskQuestion {
        AskQuestion {
            question: "which one?".to_owned(),
            options: Vec::new(),
        }
    }

    #[tokio::test]
    async fn ask_resolves_with_the_answer_the_ui_sends_back() {
        let (bridge, mut rx) = UiBridge::new();

        let responder = tokio::spawn(async move {
            let UiRequest::Ask { reply, .. } = rx.recv().await.unwrap();
            reply.send(AskAnswer::FreeText("something else".to_owned()))
        });

        let answer = bridge.ask(question()).await.unwrap();
        responder.await.unwrap().unwrap();

        assert!(matches!(answer, AskAnswer::FreeText(text) if text == "something else"));
    }

    #[tokio::test]
    async fn ask_fails_when_the_ui_is_gone() {
        let (bridge, rx) = UiBridge::new();
        drop(rx);

        assert!(bridge.ask(question()).await.is_err());
    }

    #[tokio::test]
    async fn ask_fails_when_the_question_is_dropped_unanswered() {
        let (bridge, mut rx) = UiBridge::new();

        tokio::spawn(async move {
            drop(rx.recv().await.unwrap());
        });

        assert!(bridge.ask(question()).await.is_err());
    }
}
