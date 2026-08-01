//! The multi-session pool behind `h serve`.
//!
//! Each session owns one agent running in its own task, a view-event
//! forwarder, and a task that turns the agent's [`Bridge`] ask requests into
//! `ask/question` round trips on the wire. The manager routes client requests
//! to sessions and keeps the shared bookkeeping: the pending ask ids and the
//! registry of live sessions.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use async_trait::async_trait;
use h_core::{
    event::{AgentCommand, AgentViewEvent},
    input::{Image, UserInput},
    interaction::{AskAnswer, Bridge, Request},
};
use serde_json::{Value, json};
use tokio::{
    sync::{mpsc, oneshot, Mutex},
    task::JoinHandle,
};

use crate::bootstrap::Bootstrap;

use super::protocol::{self, RpcError};

/// What a session hands back once its agent is built and running. The agent
/// itself stays inside the worker task; everything the manager needs to route
/// to and from it crosses this boundary.
pub struct SessionChannels {
    /// The id under which the session will be archived, or already was.
    pub session_id: String,
    /// The configured context window, so frontends can render the TUI-style
    /// `context current/limit` indicator against a real limit.
    pub context_window: usize,
    pub command_tx: mpsc::Sender<AgentCommand>,
    pub bus_rx: mpsc::UnboundedReceiver<AgentViewEvent>,
    pub worker: JoinHandle<anyhow::Result<()>>,
}

/// Constructs and starts one session's agent. Injectable so the serve
/// lifecycle can be tested against a scripted provider instead of a live API.
#[async_trait]
pub trait SessionBuilder: Send + Sync {
    async fn build(
        &self,
        id: Option<&str>,
        profile: Option<&str>,
        bootstrap: Bootstrap,
        bridge: Bridge,
    ) -> anyhow::Result<SessionChannels>;
}

/// The real builder: the same session assembly the TUI and headless paths use.
pub struct AgentSessionBuilder;

#[async_trait]
impl SessionBuilder for AgentSessionBuilder {
    async fn build(
        &self,
        id: Option<&str>,
        profile: Option<&str>,
        bootstrap: Bootstrap,
        bridge: Bridge,
    ) -> anyhow::Result<SessionChannels> {
        let (mut agent, context_window, mcp) =
            crate::build_agent(id, profile, bootstrap, bridge).await?;
        let session_id = agent.session_id();
        let bus_rx = agent.subscribe_view();

        agent.initialize()?;

        // A resumed session replays its history so a fresh frontend sees the
        // whole conversation, exactly like the TUI does on startup.
        if id.is_some() {
            agent.rebroadcast_all_view();
        }

        let (command_tx, command_rx) = mpsc::channel::<AgentCommand>(8);

        let worker = tokio::spawn(async move {
            agent.run(command_rx).await;

            let archived = agent.archive().await;
            let closed = mcp.close().await;

            match (archived, closed) {
                (Err(error), _) | (_, Err(error)) => {
                    tracing::error!(
                        event = "serve.session.teardown_failed",
                        error_class = "session_teardown",
                        error = error.to_string(),
                    );
                    Err(error)
                }
                _ => Ok(()),
            }
        });

        Ok(SessionChannels {
            session_id,
            context_window,
            command_tx,
            bus_rx,
            worker,
        })
    }
}

struct Session {
    command_tx: mpsc::Sender<AgentCommand>,
    context_window: usize,
    worker: JoinHandle<anyhow::Result<()>>,
    forwarder: JoinHandle<()>,
    bridge_task: JoinHandle<()>,
}

/// Routes client requests to live sessions and forwards agent output back out.
pub struct SessionManager {
    line_tx: mpsc::UnboundedSender<String>,
    builder: Box<dyn SessionBuilder>,
    sessions: HashMap<String, Session>,
    /// Ask ids in flight, keyed by the request id the client must reply to.
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<AskAnswer>>>>,
    /// One counter across all sessions, so ask ids never collide.
    next_request_id: Arc<AtomicU64>,
    default_profile: Option<String>,
}

impl SessionManager {
    pub fn new(
        line_tx: mpsc::UnboundedSender<String>,
        builder: Box<dyn SessionBuilder>,
        default_profile: Option<String>,
    ) -> Self {
        Self {
            line_tx,
            builder,
            sessions: HashMap::new(),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_request_id: Arc::new(AtomicU64::new(1)),
            default_profile,
        }
    }

    /// Handles one raw line and returns true when the server should shut down.
    pub async fn handle(&mut self, line: &str) -> anyhow::Result<bool> {
        let message: protocol::RpcIn = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(error) => {
                tracing::warn!(event = "serve.parse_error", error = %error);
                self.send(protocol::error_response(None, protocol::PARSE_ERROR, "parse error"));
                return Ok(false);
            }
        };

        if message.is_response() {
            self.route_response(&message).await;
            return Ok(false);
        }

        let Some(method) = message.method.as_deref() else {
            // A notification from the client; this protocol has none.
            tracing::warn!(event = "serve.unexpected_notification");
            return Ok(false);
        };
        let Some(id) = message.id.as_ref() else {
            tracing::warn!(event = "serve.notification_ignored", method);
            return Ok(false);
        };

        tracing::info!(event = "serve.request", method, id = %id);

        let params = message.params.unwrap_or(Value::Null);
        let outcome = match method {
            "session/create" => self.create(&params).await.map_err(map_error),
            "session/resume" => self.resume(&params).await.map_err(map_error),
            "session/list" => self.list().await.map_err(map_error),
            "session/close" => self.close(&params).await.map_err(map_error),
            "session/attach" => self.attach(&params).await.map_err(map_error),
            "turn/submit" => self.submit(&params).await.map_err(map_error),
            "turn/cancel" => self.cancel(&params).await.map_err(map_error),
            "command/run" => self.command(&params).await.map_err(map_error),
            "server/shutdown" => {
                self.send(protocol::response(id, json!({ "ok": true })));
                return Ok(true);
            }
            _ => Err(RpcError::new(
                protocol::METHOD_NOT_FOUND,
                format!("unknown method {method}"),
            )),
        };

        match outcome {
            Ok(result) => self.send(protocol::response(id, result)),
            Err(error) => {
                tracing::warn!(
                    event = "serve.request_failed",
                    method,
                    error_class = "rpc_error",
                    error = %error.message,
                );
                self.send(protocol::error_response(Some(id), error.code, &error.message));
            }
        }

        Ok(false)
    }

    async fn create(&mut self, params: &Value) -> anyhow::Result<Value> {
        let profile = params.get("profile").and_then(Value::as_str);
        let bootstrap = match params.get("instruction").and_then(Value::as_str) {
            Some(instruction) => Bootstrap::Instruction(instruction.to_owned()),
            None => Bootstrap::Default,
        };

        let (session_id, context_window) = self.build_session(None, profile, bootstrap).await?;
        Ok(json!({ "session_id": session_id, "context_window": context_window }))
    }

    async fn resume(&mut self, params: &Value) -> anyhow::Result<Value> {
        let id = required_str(params, "session_id")?;
        let (session_id, context_window) =
            self.build_session(Some(id), None, Bootstrap::Default).await?;
        Ok(json!({ "session_id": session_id, "context_window": context_window }))
    }

    async fn build_session(
        &mut self,
        id: Option<&str>,
        profile: Option<&str>,
        bootstrap: Bootstrap,
    ) -> anyhow::Result<(String, usize)> {
        let (bridge, interaction_rx) = Bridge::new();
        let channels = self
            .builder
            .build(id, profile.or(self.default_profile.as_deref()), bootstrap, bridge)
            .await?;

        let session_id = channels.session_id.clone();
        let context_window = channels.context_window;

        let line_tx = self.line_tx.clone();
        let session_event_session_id = session_id.clone();
        let forwarder = tokio::spawn(async move {
            let mut bus_rx = channels.bus_rx;
            while let Some(event) = bus_rx.recv().await {
                let line = match event {
                    AgentViewEvent::Startup {
                        model,
                        thinking_effort,
                    } => protocol::session_started(
                        &session_event_session_id,
                        &model,
                        thinking_effort.as_deref(),
                        context_window,
                    ),
                    other => protocol::session_event(&session_event_session_id, &other),
                };
                if line_tx.send(line).is_err() {
                    break;
                }
            }
        });

        let line_tx = self.line_tx.clone();
        let pending = self.pending.clone();
        let next_request_id = self.next_request_id.clone();
        let ask_session_id = session_id.clone();
        let bridge_task = tokio::spawn(async move {
            let mut interaction_rx = interaction_rx;
            while let Some(request) = interaction_rx.recv().await {
                let Request::Ask { question, reply } = request;
                let id = next_request_id.fetch_add(1, Ordering::SeqCst);

                pending.lock().await.insert(id, reply);
                let line = protocol::ask_question(id, &ask_session_id, &question);
                if line_tx.send(line).is_err() {
                    break;
                }
            }
        });

        self.sessions.insert(
            session_id.clone(),
            Session {
                command_tx: channels.command_tx,
                context_window,
                worker: channels.worker,
                forwarder,
                bridge_task,
            },
        );

        tracing::info!(event = "serve.session.started", session_id);

        Ok((session_id, context_window))
    }

    async fn list(&self) -> anyhow::Result<Value> {
        let archived = h_core::context::list_sessions()
            .await?
            .into_iter()
            .map(|session| {
                json!({
                    "id": session.id,
                    "title": session.title,
                    "last_modified": session.last_modified,
                })
            })
            .collect::<Vec<_>>();
        let active = self
            .sessions
            .keys()
            .map(|id| json!({ "id": id }))
            .collect::<Vec<_>>();

        Ok(json!({ "archived": archived, "active": active }))
    }

    async fn close(&mut self, params: &Value) -> anyhow::Result<Value> {
        let id = required_str(params, "session_id")?;
        let Some(session) = self.sessions.remove(id) else {
            return Err(session_not_found(id).into());
        };

        // Dropping the command sender lets the worker finish its turn, archive,
        // and exit; then the forwarder and ask tasks drain as the agent drops.
        drop(session.command_tx);
        let _ = session.worker.await;
        let _ = session.forwarder.await;
        let _ = session.bridge_task.await;

        tracing::info!(event = "serve.session.closed", session_id = id);

        Ok(json!({ "archived": true }))
    }

    async fn attach(&self, params: &Value) -> anyhow::Result<Value> {
        let id = required_str(params, "session_id")?;
        let session = self
            .sessions
            .get(id)
            .ok_or_else(|| session_not_found(id))?;

        session
            .command_tx
            .send(AgentCommand::Rebroadcast)
            .await
            .map_err(|_| session_not_found(id))?;

        Ok(json!({
            "replayed": true,
            "context_window": session.context_window,
        }))
    }

    async fn submit(&self, params: &Value) -> anyhow::Result<Value> {
        let id = required_str(params, "session_id")?;
        let session = self
            .sessions
            .get(id)
            .ok_or_else(|| session_not_found(id))?;

        let text = params.get("text").and_then(Value::as_str).unwrap_or_default();
        let mut images = Vec::new();

        if let Some(items) = params.get("images").and_then(Value::as_array) {
            for item in items {
                let image = Image::from_base64(
                    required_str(item, "media_type")?.to_owned(),
                    required_str(item, "data")?.to_owned(),
                    item.get("width").and_then(Value::as_u64).unwrap_or(0) as u32,
                    item.get("height").and_then(Value::as_u64).unwrap_or(0) as u32,
                )?;
                images.push(image);
            }
        }

        session
            .command_tx
            .send(AgentCommand::Prompt(UserInput::from_text_and_images(
                text.to_owned(),
                images,
            )))
            .await
            .map_err(|_| session_not_found(id))?;

        Ok(json!({ "accepted": true }))
    }

    async fn cancel(&self, params: &Value) -> anyhow::Result<Value> {
        let id = required_str(params, "session_id")?;
        let session = self
            .sessions
            .get(id)
            .ok_or_else(|| session_not_found(id))?;

        session
            .command_tx
            .send(AgentCommand::Cancel)
            .await
            .map_err(|_| session_not_found(id))?;

        Ok(json!({ "accepted": true }))
    }

    async fn command(&self, params: &Value) -> anyhow::Result<Value> {
        let id = required_str(params, "session_id")?;
        let session = self
            .sessions
            .get(id)
            .ok_or_else(|| session_not_found(id))?;
        let command = required_str(params, "command")?;
        let command = h_core::command::Command::parse(command)
            .ok_or_else(|| RpcError::new(protocol::INVALID_PARAMS, format!("unknown command {command}")))?;

        session
            .command_tx
            .send(AgentCommand::Run(command))
            .await
            .map_err(|_| session_not_found(id))?;

        Ok(json!({ "accepted": true }))
    }

    /// Resolves an in-flight ask with the client's answer.
    async fn route_response(&mut self, message: &protocol::RpcIn) {
        let Some(id) = message.id.as_ref().and_then(Value::as_u64) else {
            return;
        };
        let Some(answer) = message
            .result
            .as_ref()
            .and_then(|result| result.get("answer"))
            .and_then(|answer| serde_json::from_value::<AskAnswer>(answer.clone()).ok())
        else {
            tracing::warn!(event = "serve.ask_answer_malformed", id);
            return;
        };

        if let Some(reply) = self.pending.lock().await.remove(&id) {
            let _ = reply.send(answer);
            tracing::info!(event = "serve.ask.answered", id);
        }
    }

    /// Closes every session. In-flight asks fail fast so turns can unwind;
    /// each worker finishes, archives, and exits.
    pub async fn shutdown(&mut self) {
        self.pending.lock().await.clear();

        for (id, session) in self.sessions.drain() {
            session.bridge_task.abort();
            drop(session.command_tx);
            let _ = session.worker.await;
            let _ = session.forwarder.await;
            tracing::info!(event = "serve.session.stopped", session_id = id);
        }
    }

    fn send(&self, line: String) {
        if self.line_tx.send(line).is_err() {
            tracing::warn!(event = "serve.output_closed");
        }
    }
}

fn required_str<'a>(params: &'a Value, key: &str) -> Result<&'a str, RpcError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new(protocol::INVALID_PARAMS, format!("missing string field {key}")))
}

fn session_not_found(id: &str) -> RpcError {
    RpcError::new(
        protocol::SESSION_NOT_FOUND,
        format!("no live session {id}"),
    )
}

/// Coarse error mapping; the resume refusal message is stable enough to match.
fn map_error(error: anyhow::Error) -> RpcError {
    let message = error.to_string();
    if message.contains("refusing to resume") {
        RpcError::new(protocol::RESUME_REFUSED, message)
    } else {
        RpcError::new(protocol::INIT_ERROR, message)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        path::PathBuf,
        time::Duration,
    };

    use futures::stream;
    use h_core::{
        agent::Agent,
        context::Message,
        event::{CompletedReason, ProviderSignal},
        provider::{Provider, ProviderEventStream},
        tool::ToolDefinition,
    };
    use serde_json::json;

    use super::*;

    fn test_manager(rounds: Vec<Vec<ProviderSignal>>) -> (SessionManager, mpsc::UnboundedReceiver<String>, PathBuf) {
        let archive = std::env::temp_dir().join(format!("h-serve-test-{}", uuid::Uuid::new_v4()));
        let (line_tx, line_rx) = mpsc::unbounded_channel::<String>();
        let manager = SessionManager::new(
            line_tx,
            Box::new(FakeBuilder {
                rounds: Arc::new(Mutex::new(VecDeque::from(rounds))),
                archive_dir: archive.clone(),
            }),
            None,
        );
        (manager, line_rx, archive)
    }

    /// Waits for the next line whose predicate matches, skipping others.
    async fn next_matching(
        rx: &mut mpsc::UnboundedReceiver<String>,
        predicate: impl Fn(&Value) -> bool,
    ) -> Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let line = rx.recv().await.expect("output channel closed");
                let value: Value = serde_json::from_str(&line).unwrap();
                if predicate(&value) {
                    return value;
                }
            }
        })
        .await
        .expect("timed out waiting for a matching message")
    }

    fn is_response_to(value: &Value, id: u64) -> bool {
        value.get("id").and_then(Value::as_u64) == Some(id) && value.get("result").is_some()
    }

    fn event_of(value: &Value, event_type: &str) -> bool {
        value.get("method").and_then(Value::as_str) == Some("session/event")
            && value["params"]["event"]["type"] == event_type
    }

    #[tokio::test]
    async fn create_streams_startup_and_submit_streams_the_reply() {
        let (mut manager, mut rx, archive) = test_manager(vec![vec![
            ProviderSignal::TextDelta("hello from the fake".to_owned()),
            ProviderSignal::Completed {
                reason: CompletedReason::Final,
            },
        ]]);

        manager
            .handle(r#"{"jsonrpc":"2.0","id":1,"method":"session/create","params":{}}"#)
            .await
            .unwrap();

        let create_reply = next_matching(&mut rx, |value| is_response_to(value, 1)).await;
        assert_eq!(create_reply["result"]["context_window"], 100_000);
        let session_id = create_reply["result"]["session_id"]
            .as_str()
            .unwrap()
            .to_owned();

        let started = next_matching(&mut rx, |value| {
            value.get("method").and_then(Value::as_str) == Some("session/started")
        })
        .await;
        assert_eq!(started["params"]["session_id"], session_id);
        assert_eq!(started["params"]["model"], "fake-model");
        assert_eq!(started["params"]["context_window"], 100_000);

        manager
            .handle(&format!(
                r#"{{"jsonrpc":"2.0","id":2,"method":"turn/submit","params":{{"session_id":"{session_id}","text":"hi"}}}}"#
            ))
            .await
            .unwrap();
        assert!(
            next_matching(&mut rx, |value| is_response_to(value, 2)).await["result"]["accepted"]
                == true
        );

        let delta = next_matching(&mut rx, |value| event_of(value, "text_delta")).await;
        assert_eq!(delta["params"]["event"]["data"], "hello from the fake");
        assert!(
            next_matching(&mut rx, |value| event_of(value, "turn_finished")).await["params"]["event"]
                ["data"]["completed"]
                == true
        );

        drop(manager);
        drop(rx);
        let _ = std::fs::remove_dir_all(archive);
    }

    #[tokio::test]
    async fn ask_question_round_trips_through_the_bridge() {
        let (mut manager, mut rx, archive) = test_manager(vec![
            vec![
                ProviderSignal::ToolCallStarted(h_core::tool::ToolCall::new(
                    "call-1",
                    "ask",
                    json!({
                        "question": "which one?",
                        "options": [{ "label": "first" }, { "label": "second" }],
                    }),
                )),
                ProviderSignal::Completed {
                    reason: CompletedReason::NeedCall,
                },
            ],
            vec![
                ProviderSignal::TextDelta("the user picked first".to_owned()),
                ProviderSignal::Completed {
                    reason: CompletedReason::Final,
                },
            ],
        ]);

        manager
            .handle(r#"{"jsonrpc":"2.0","id":1,"method":"session/create","params":{}}"#)
            .await
            .unwrap();
        let session_id = next_matching(&mut rx, |value| is_response_to(value, 1)).await["result"]
            ["session_id"]
            .as_str()
            .unwrap()
            .to_owned();

        manager
            .handle(&format!(
                r#"{{"jsonrpc":"2.0","id":2,"method":"turn/submit","params":{{"session_id":"{session_id}","text":"choose"}}}}"#
            ))
            .await
            .unwrap();

        let ask = next_matching(&mut rx, |value| {
            value.get("method").and_then(Value::as_str) == Some("ask/question")
        })
        .await;
        let ask_id = ask["id"].as_u64().unwrap();
        assert_eq!(ask["params"]["question"], "which one?");
        assert_eq!(ask["params"]["options"][0]["label"], "first");

        manager
            .handle(&format!(
                r#"{{"jsonrpc":"2.0","id":{ask_id},"result":{{"answer":{{"type":"option","data":{{"index":0,"label":"first"}}}}}}}}"#
            ))
            .await
            .unwrap();

        let delta = next_matching(&mut rx, |value| event_of(value, "text_delta")).await;
        assert_eq!(delta["params"]["event"]["data"], "the user picked first");

        drop(manager);
        drop(rx);
        let _ = std::fs::remove_dir_all(archive);
    }

    #[tokio::test]
    async fn close_archives_and_removes_the_session() {
        let (mut manager, mut rx, archive) = test_manager(vec![vec![
            ProviderSignal::TextDelta("one turn".to_owned()),
            ProviderSignal::Completed {
                reason: CompletedReason::Final,
            },
        ]]);

        manager
            .handle(r#"{"jsonrpc":"2.0","id":1,"method":"session/create","params":{}}"#)
            .await
            .unwrap();
        let session_id = next_matching(&mut rx, |value| is_response_to(value, 1)).await["result"]
            ["session_id"]
            .as_str()
            .unwrap()
            .to_owned();

        manager
            .handle(&format!(
                r#"{{"jsonrpc":"2.0","id":2,"method":"turn/submit","params":{{"session_id":"{session_id}","text":"work"}}}}"#
            ))
            .await
            .unwrap();
        // The submit reply lands before the turn's streamed events.
        _ = next_matching(&mut rx, |value| is_response_to(value, 2)).await;
        next_matching(&mut rx, |value| event_of(value, "turn_finished")).await;

        manager
            .handle(&format!(
                r#"{{"jsonrpc":"2.0","id":3,"method":"session/close","params":{{"session_id":"{session_id}"}}}}"#
            ))
            .await
            .unwrap();

        let close_reply = next_matching(&mut rx, |value| is_response_to(value, 3)).await;
        assert_eq!(close_reply["result"]["archived"], true);

        manager
            .handle(r#"{"jsonrpc":"2.0","id":4,"method":"session/list","params":{}}"#)
            .await
            .unwrap();
        let list = next_matching(&mut rx, |value| is_response_to(value, 4)).await;
        assert!(list["result"]["active"].as_array().unwrap().is_empty());

        // The turn had an exchange, so closing archived it into the temp dir.
        assert!(!archive.read_dir().unwrap().next().is_none());

        drop(manager);
        drop(rx);
        let _ = std::fs::remove_dir_all(archive);
    }

    #[tokio::test]
    async fn attach_replays_the_transcript() {
        let (mut manager, mut rx, archive) = test_manager(vec![vec![
            ProviderSignal::TextDelta("the answer".to_owned()),
            ProviderSignal::Completed {
                reason: CompletedReason::Final,
            },
        ]]);

        manager
            .handle(r#"{"jsonrpc":"2.0","id":1,"method":"session/create","params":{}}"#)
            .await
            .unwrap();
        let session_id = next_matching(&mut rx, |value| is_response_to(value, 1)).await["result"]
            ["session_id"]
            .as_str()
            .unwrap()
            .to_owned();

        manager
            .handle(&format!(
                r#"{{"jsonrpc":"2.0","id":2,"method":"turn/submit","params":{{"session_id":"{session_id}","text":"work"}}}}"#
            ))
            .await
            .unwrap();
        // The submit reply lands before the turn's streamed events.
        _ = next_matching(&mut rx, |value| is_response_to(value, 2)).await;
        next_matching(&mut rx, |value| event_of(value, "turn_finished")).await;

        manager
            .handle(&format!(
                r#"{{"jsonrpc":"2.0","id":3,"method":"session/attach","params":{{"session_id":"{session_id}"}}}}"#
            ))
            .await
            .unwrap();
        let attach_reply = next_matching(&mut rx, |value| is_response_to(value, 3)).await;
        assert!(attach_reply["result"]["replayed"] == true);
        assert_eq!(attach_reply["result"]["context_window"], 100_000);

        // The replay carries the user prompt and the assistant reply back out.
        let prompt = next_matching(&mut rx, |value| event_of(value, "prompt")).await;
        assert_eq!(prompt["params"]["event"]["data"], "work");
        assert!(
            next_matching(&mut rx, |value| event_of(value, "text_delta")).await["params"]["event"]
                ["data"]
                == "the answer"
        );

        drop(manager);
        drop(rx);
        let _ = std::fs::remove_dir_all(archive);
    }

    #[tokio::test]
    async fn shutdown_closes_every_session() {
        let (mut manager, mut rx, archive) = test_manager(vec![vec![
            ProviderSignal::ToolCallStarted(h_core::tool::ToolCall::new(
                "call-1",
                "ask",
                json!({ "question": "block forever?", "options": [{ "label": "wait" }] }),
            )),
            ProviderSignal::Completed {
                reason: CompletedReason::NeedCall,
            },
        ]]);

        manager
            .handle(r#"{"jsonrpc":"2.0","id":1,"method":"session/create","params":{}}"#)
            .await
            .unwrap();
        let session_id = next_matching(&mut rx, |value| is_response_to(value, 1)).await["result"]
            ["session_id"]
            .as_str()
            .unwrap()
            .to_owned();

        manager
            .handle(&format!(
                r#"{{"jsonrpc":"2.0","id":2,"method":"turn/submit","params":{{"session_id":"{session_id}","text":"stuck"}}}}"#
            ))
            .await
            .unwrap();
        // Let the ask reach the wire before shutting down.
        next_matching(&mut rx, |value| {
            value.get("method").and_then(Value::as_str) == Some("ask/question")
        })
        .await;

        // Shutdown must not hang: the in-flight ask fails fast and the worker
        // unwinds without waiting for an answer.
        tokio::time::timeout(Duration::from_secs(5), manager.shutdown())
            .await
            .expect("shutdown hung on a session blocked in ask");
        assert!(manager.sessions.is_empty());

        drop(rx);
        let _ = std::fs::remove_dir_all(archive);
    }

    /// Builds sessions from a scripted provider, so no live API is involved.
    struct FakeBuilder {
        rounds: Arc<Mutex<VecDeque<Vec<ProviderSignal>>>>,
        archive_dir: PathBuf,
    }

    #[async_trait]
    impl SessionBuilder for FakeBuilder {
        async fn build(
            &self,
            _id: Option<&str>,
            _profile: Option<&str>,
            _bootstrap: Bootstrap,
            bridge: Bridge,
        ) -> anyhow::Result<SessionChannels> {
            let mut agent = Agent::new(ScriptedProvider {
                rounds: self.rounds.clone(),
            });
            agent.with_archive_dir(&self.archive_dir);
            agent.with_internal_tools(bridge)?;

            let session_id = agent.session_id();
            let bus_rx = agent.subscribe_view();
            agent.initialize()?;

            let (command_tx, command_rx) = mpsc::channel::<AgentCommand>(8);
            let worker = tokio::spawn(async move {
                agent.run(command_rx).await;
                agent.archive().await
            });

            Ok(SessionChannels {
                session_id,
                context_window: 100_000,
                command_tx,
                bus_rx,
                worker,
            })
        }
    }

    struct ScriptedProvider {
        rounds: Arc<Mutex<VecDeque<Vec<ProviderSignal>>>>,
    }

    #[async_trait]
    impl Provider for ScriptedProvider {
        fn model(&self) -> &str {
            "fake-model"
        }

        fn thinking_effort(&self) -> Option<&str> {
            None
        }

        fn define_tools(&mut self, _specs: Vec<ToolDefinition>) -> anyhow::Result<()> {
            Ok(())
        }

        async fn stream(&self, _input: &[Message]) -> anyhow::Result<ProviderEventStream> {
            let round = self
                .rounds
                .lock()
                .await
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("no scripted provider round left"))?;

            Ok(Box::pin(stream::iter(round.into_iter().map(Ok))))
        }
    }
}
