use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

/// Requests queue here while the user works through them, so a burst never
/// blocks the caller before the interaction handler has drained it.
const REQUEST_CAPACITY: usize = 8;

/// A question that requires an answer from outside the agent.
#[derive(Debug, Clone, Serialize)]
pub struct AskQuestion {
    pub question: String,
    pub options: Vec<AskOption>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AskOption {
    pub label: String,
    pub description: Option<String>,
}

/// The reply to an [`AskQuestion`]. `Option` carries the index into its options;
/// `FreeText` is what was written when none of them fit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum AskAnswer {
    Option { index: usize, label: String },
    FreeText(String),
}

/// A request that an external interaction handler must resolve.
///
/// This is point-to-point because each request owns one reply channel. The
/// responder keeps the answer attached to the operation that requested it.
#[derive(Debug)]
pub enum Request {
    Ask {
        question: AskQuestion,
        reply: oneshot::Sender<AskAnswer>,
    },
}

/// The caller-side half of an external interaction round trip.
///
/// Unlike [`EventBus`](crate::bus::EventBus), which broadcasts clonable events
/// one way, this carries a reply channel per request and is point-to-point.
/// Hand a clone to anything that needs an external answer and the receiver to
/// whichever frontend or host integration provides it.
#[derive(Clone)]
pub struct Bridge {
    tx: mpsc::Sender<Request>,
}

impl Bridge {
    pub fn new() -> (Self, mpsc::Receiver<Request>) {
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
            .send(Request::Ask { question, reply })
            .await
            .is_err()
        {
            tracing::warn!(
                event = "interaction.request.failed",
                operation = "send",
                error_class = "handler_unavailable"
            );
            anyhow::bail!("no interaction handler is available to answer the question");
        }

        answer.await.map_err(|_| {
            tracing::warn!(
                event = "interaction.request.failed",
                operation = "await_reply",
                error_class = "reply_dropped"
            );
            anyhow::anyhow!("the question was dismissed without an answer")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{AskAnswer, AskQuestion, Bridge, Request};

    fn question() -> AskQuestion {
        AskQuestion {
            question: "which one?".to_owned(),
            options: Vec::new(),
        }
    }

    #[tokio::test]
    async fn ask_resolves_with_the_answer_the_ui_sends_back() {
        let (bridge, mut rx) = Bridge::new();

        let responder = tokio::spawn(async move {
            let Request::Ask { reply, .. } = rx.recv().await.unwrap();
            reply.send(AskAnswer::FreeText("something else".to_owned()))
        });

        let answer = bridge.ask(question()).await.unwrap();
        responder.await.unwrap().unwrap();

        assert!(matches!(answer, AskAnswer::FreeText(text) if text == "something else"));
    }

    #[tokio::test]
    async fn ask_fails_when_the_ui_is_gone() {
        let (bridge, rx) = Bridge::new();
        drop(rx);

        assert!(bridge.ask(question()).await.is_err());
    }

    #[tokio::test]
    async fn ask_fails_when_the_question_is_dropped_unanswered() {
        let (bridge, mut rx) = Bridge::new();

        tokio::spawn(async move {
            drop(rx.recv().await.unwrap());
        });

        assert!(bridge.ask(question()).await.is_err());
    }

    #[test]
    fn ask_answers_round_trip_through_json() {
        let option = AskAnswer::Option {
            index: 0,
            label: "run".to_owned(),
        };
        let free_text = AskAnswer::FreeText("do it".to_owned());

        for answer in [option.clone(), free_text.clone()] {
            let wire = serde_json::to_value(&answer).unwrap();
            let back: AskAnswer = serde_json::from_value(wire).unwrap();
            assert_eq!(back, answer);
        }

        assert_eq!(
            serde_json::to_value(&option).unwrap(),
            serde_json::json!({"type": "option", "data": {"index": 0, "label": "run"}})
        );
        assert_eq!(
            serde_json::to_value(&free_text).unwrap(),
            serde_json::json!({"type": "free_text", "data": "do it"})
        );
    }
}
