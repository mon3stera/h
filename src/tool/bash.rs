use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use dashmap::{
    DashMap,
    mapref::one::{Ref, RefMut},
};
use expectrl::{
    AsyncExpect, Session,
    process::unix::{AsyncPtyStream, UnixProcess},
    repl::ReplSession,
    stream::log::{self, LogStream},
};
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{
    fs::{self, OpenOptions},
    io::AsyncWriteExt,
    process::Command,
};
use uuid::Uuid;

use super::{
    DisplayBlock, KeyValueEntry, Presentation, Presenter, ToolCall, ToolCallOutcome,
    ToolCallResult, ToolCallStatus, TypedTool,
    presentation::{
        MAX_ERROR_CHARS, MAX_FIELD_CHARS, truncate_chars, truncate_preview, value_to_display_block,
    },
};

const MAX_INLINE_OUTPUT_BYTES: usize = 512;

fn preview_end(content: &str) -> usize {
    (0..=MAX_INLINE_OUTPUT_BYTES.min(content.len()))
        .rev()
        .find(|index| content.is_char_boundary(*index))
        .unwrap_or(0)
}

async fn write_temp(content: impl AsRef<str>) -> anyhow::Result<String> {
    let path = PathBuf::from(format!("/tmp/h_{}", Uuid::new_v4()));
    fs::write(&path, content.as_ref()).await?;
    Ok(path.display().to_string())
}

async fn write_temp_and_mention(content: impl AsRef<str>) -> anyhow::Result<String> {
    let content = content.as_ref();

    if content.len() < MAX_INLINE_OUTPUT_BYTES {
        return Ok(content.to_owned());
    }

    let path = write_temp(content).await?;
    let preview = &content[..preview_end(content)];

    Ok(format!(
        "{preview}... [Truncated] (Find full contents in {path})"
    ))
}

fn truncate_session_output(output: String, history_file: &Path, suffix: &str) -> String {
    if output.len() < MAX_INLINE_OUTPUT_BYTES {
        return output;
    }

    let preview = &output[..preview_end(&output)];

    format!(
        "{preview} [Truncated] (Read {} {suffix})",
        history_file.display()
    )
}

#[derive(Debug, Clone)]
struct MemoryLog {
    history_file: PathBuf,
    inner: Arc<Mutex<String>>,
}

impl MemoryLog {
    fn new() -> Self {
        Self {
            history_file: PathBuf::from(format!("/tmp/h_{}", Uuid::new_v4())),
            inner: Arc::new(Mutex::new(String::new())),
        }
    }
}

impl Write for MemoryLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut inner = self.inner.lock();
        let cleaned = strip_ansi_escapes::strip(buf);

        inner.push_str(&String::from_utf8_lossy(&cleaned));
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct BashSession {
    id: String,
    session: ReplSession<Session<UnixProcess, LogStream<AsyncPtyStream, MemoryLog>>>,
    memory: MemoryLog,
    is_busy: bool,
}

enum SpawnResult {
    Ok,
    Busy,
}

enum KillResult {
    Ok,
    NoBusyCommand,
}

enum WaitResult {
    Ok { output: String },
    NoBusyCommand,
}

enum ViewResult {
    Ok { output: String },
    NoBusyCommand,
}

impl BashSession {
    async fn kill(&mut self) -> anyhow::Result<KillResult> {
        if !self.is_busy {
            return Ok(KillResult::NoBusyCommand);
        }

        self.session.send(&[3]).await?;
        Ok(KillResult::Ok)
    }

    async fn archive_log(&mut self) -> anyhow::Result<String> {
        let content = self.memory.inner.lock().clone();
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .append(true)
            .open(&self.memory.history_file)
            .await?;
        let content = strip_ansi_escapes::strip(content);

        file.write_all(&content).await?;
        self.memory.inner.lock().clear();
        Ok(String::from_utf8_lossy(&content).into_owned())
    }

    async fn view(&mut self) -> anyhow::Result<ViewResult> {
        if !self.is_busy {
            return Ok(ViewResult::NoBusyCommand);
        }

        let output = self.archive_log().await?;
        let output = truncate_session_output(
            output,
            &self.memory.history_file,
            "to find all inputs and outputs of the session",
        );

        Ok(ViewResult::Ok { output })
    }

    async fn send(&mut self, buf: &[u8]) -> anyhow::Result<()> {
        self.session.send(buf).await?;
        Ok(())
    }

    fn log_file(&self) -> String {
        self.memory.history_file.display().to_string()
    }

    async fn wait(&mut self) -> anyhow::Result<WaitResult> {
        if !self.is_busy {
            return Ok(WaitResult::NoBusyCommand);
        }

        self.session.expect_prompt().await?;
        self.is_busy = false;

        let output = self.archive_log().await?;
        let output = truncate_session_output(
            output,
            &self.memory.history_file,
            "to find all inputs and outputs",
        );

        Ok(WaitResult::Ok { output })
    }

    async fn spawn(&mut self, command: impl AsRef<str>) -> anyhow::Result<SpawnResult> {
        let command = command.as_ref();

        if self.is_busy {
            return Ok(SpawnResult::Busy);
        }

        self.is_busy = true;
        self.session.send_line(command).await?;
        Ok(SpawnResult::Ok)
    }
}

pub struct BashTool {
    sessions: DashMap<String, BashSession>,
}

impl BashTool {
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
        }
    }

    async fn spawn(&self) -> anyhow::Result<String> {
        const DEFAULT_PROMPT: &str = "EXPECT_PROMPT";

        let (session_id, memory_log) = (Uuid::new_v4().to_string(), MemoryLog::new());
        let mut command = std::process::Command::new("bash");

        command.env("PS1", DEFAULT_PROMPT);
        command.env(
            "PROMPT_COMMAND",
            "PS1=EXPECT_PROMPT; unset PROMPT_COMMAND; bind 'set enable-bracketed-paste off'",
        );

        let session = expectrl::session::Session::spawn(command)?;
        let session = expectrl::session::log(session, memory_log.clone())?;
        let mut bash: ReplSession<
            Session<
                UnixProcess,
                log::LogStream<expectrl::process::unix::AsyncPtyStream, MemoryLog>,
            >,
        > = ReplSession::new(session, DEFAULT_PROMPT);

        bash.set_quit_command("quit");
        bash.expect_prompt().await?;

        self.sessions.insert(
            session_id.clone(),
            BashSession {
                id: session_id.clone(),
                session: bash,
                memory: memory_log,
                is_busy: false,
            },
        );

        Ok(session_id)
    }

    async fn get_mut(
        &self,
        session_id: impl AsRef<str>,
    ) -> Option<RefMut<'_, String, BashSession>> {
        self.sessions.get_mut(session_id.as_ref())
    }

    async fn get(&self, session_id: impl AsRef<str>) -> Option<Ref<'_, String, BashSession>> {
        self.sessions.get(session_id.as_ref())
    }

    async fn get_or_spawn(
        &self,
        session_id: Option<String>,
    ) -> anyhow::Result<RefMut<'_, String, BashSession>> {
        if let Some(session_id) = session_id {
            return Ok(self.sessions.get_mut(&session_id).unwrap());
        }

        let session_id = self.spawn().await?;
        Ok(self.sessions.get_mut(&session_id).unwrap())
    }

    async fn run_background(
        &self,
        session_id: Option<String>,
        command: String,
    ) -> anyhow::Result<BashToolOutput> {
        if let Some(session_id) = &session_id
            && !self.sessions.contains_key(session_id)
        {
            return Ok(BashToolOutput::SessionNotExist);
        }

        let mut session = self.get_or_spawn(session_id).await?;

        match session.spawn(command).await? {
            SpawnResult::Ok => {}
            SpawnResult::Busy => return Ok(BashToolOutput::SessionBusy),
        }

        Ok(BashToolOutput::Spawned {
            session_id: session.id.clone(),
        })
    }

    async fn view(&self, session_id: String) -> anyhow::Result<BashToolOutput> {
        let mut session = match self.get_mut(&session_id).await {
            Some(session) => session,
            None => return Ok(BashToolOutput::SessionNotExist),
        };

        match session.view().await? {
            ViewResult::NoBusyCommand => Ok(BashToolOutput::NoBusyCommand),
            ViewResult::Ok { output } => Ok(BashToolOutput::Output { output }),
        }
    }

    async fn wait(&self, session_id: String) -> anyhow::Result<BashToolOutput> {
        let mut session = match self.get_mut(&session_id).await {
            Some(session) => session,
            None => return Ok(BashToolOutput::SessionNotExist),
        };

        match session.wait().await? {
            WaitResult::NoBusyCommand => Ok(BashToolOutput::NoBusyCommand),
            WaitResult::Ok { output } => Ok(BashToolOutput::Output { output }),
        }
    }

    async fn terminate(&self, session_id: String) -> anyhow::Result<BashToolOutput> {
        let mut session = match self.get_mut(&session_id).await {
            Some(session) => session,
            None => return Ok(BashToolOutput::SessionNotExist),
        };

        match session.kill().await? {
            KillResult::Ok => Ok(BashToolOutput::Terminated),
            KillResult::NoBusyCommand => Ok(BashToolOutput::NoBusyCommand),
        }
    }

    async fn log_path(&self, session_id: String) -> BashToolOutput {
        let session = match self.get(&session_id).await {
            Some(session) => session,
            None => return BashToolOutput::SessionNotExist,
        };

        BashToolOutput::FilePath {
            path: session.log_file(),
        }
    }

    async fn send(
        &self,
        session_id: String,
        input: impl AsRef<[u8]>,
    ) -> anyhow::Result<BashToolOutput> {
        let input = input.as_ref();
        let mut session = match self.get_mut(&session_id).await {
            Some(session) => session,
            None => return Ok(BashToolOutput::SessionNotExist),
        };

        session.send(input).await?;
        Ok(BashToolOutput::Sent)
    }

    async fn run_blocking(&self, command: String) -> anyhow::Result<BashToolOutput> {
        if command.split_whitespace().next().is_none() {
            anyhow::bail!("Empty command");
        }

        let output = Command::new("bash").arg("-c").arg(command).output().await?;
        let (stdout, stderr) = (
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        let (stdout, stderr) = (
            write_temp_and_mention(stdout).await?,
            write_temp_and_mention(stderr).await?,
        );

        Ok(BashToolOutput::RanBlocking { stdout, stderr })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum BashToolArgs {
    /// Run a command and block until it completes.
    RunBlocking {
        /// The shell command to execute.
        command: String,
    },
    /// Spawn a command in the background without waiting for completion.
    RunBackground {
        /// The shell command to execute.
        command: String,
        /// An existing background terminal session. A new session is created when omitted.
        session_id: Option<String>,
    },
    /// Get the log file path containing a session's historical inputs and outputs.
    LogFilePath {
        /// The background terminal session to inspect.
        session_id: String,
    },
    /// Send input or a command to a running background terminal session.
    Send {
        /// The background terminal session to receive the input.
        session_id: String,
        /// The text or command to send.
        input: String,
    },
    /// Get the buffered output generated since the last view for a running session.
    View {
        /// The background terminal session to inspect.
        session_id: String,
    },
    /// Wait until the running command in a background terminal session exits.
    Wait {
        /// The background terminal session to wait for.
        session_id: String,
    },
    /// Kill the running command in a background terminal session.
    Terminate {
        /// The background terminal session whose command should be killed.
        session_id: String,
    },
}

#[async_trait::async_trait]
impl TypedTool for BashTool {
    type Arguments = BashToolArgs;
    type Output = BashToolOutput;

    fn name(&self) -> &'static str {
        "bash"
    }

    fn description(&self) -> &'static str {
        "Execute bash commands and manage background terminal sessions."
    }

    async fn call(&self, args: Self::Arguments) -> anyhow::Result<Self::Output> {
        match args {
            BashToolArgs::RunBlocking { command } => self.run_blocking(command).await,
            BashToolArgs::RunBackground {
                command,
                session_id,
            } => self.run_background(session_id, command).await,
            BashToolArgs::LogFilePath { session_id } => Ok(self.log_path(session_id).await),
            BashToolArgs::Send { session_id, input } => self.send(session_id, input).await,
            BashToolArgs::View { session_id } => self.view(session_id).await,
            BashToolArgs::Wait { session_id } => self.wait(session_id).await,
            BashToolArgs::Terminate { session_id } => self.terminate(session_id).await,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum BashToolOutput {
    FilePath { path: String },
    Sent,
    RanBlocking { stdout: String, stderr: String },
    Spawned { session_id: String },
    Output { output: String },
    NoBusyCommand,
    Terminated,
    SessionBusy,
    SessionNotExist,
}

pub struct BashPresenter;

impl BashPresenter {
    fn action(call: &ToolCall) -> Option<&str> {
        call.arguments.get("action").and_then(Value::as_str)
    }

    fn target(call: &ToolCall) -> Option<String> {
        let key = match Self::action(call) {
            Some("run_blocking" | "run_background") => "command",
            Some("log_file_path" | "send" | "view" | "wait" | "terminate") => "session_id",
            _ => return None,
        };

        let target = call
            .arguments
            .get(key)
            .and_then(Value::as_str)
            .filter(|target| !target.is_empty())?;
        let target = strip_ansi_escapes::strip(target.as_bytes());
        let target = String::from_utf8_lossy(&target)
            .replace('\r', "\\r")
            .replace('\n', "\\n")
            .replace('\t', "\\t");

        Some(truncate_chars(&target, MAX_FIELD_CHARS))
    }

    fn presentation(
        call: &ToolCall,
        status: ToolCallStatus,
        blocks: Vec<DisplayBlock>,
    ) -> Presentation {
        Presentation {
            call_id: call.id.clone(),
            name: "Bash".to_owned(),
            label: "built-in".to_owned(),
            target: Self::target(call),
            status,
            blocks,
        }
    }

    fn running_blocks(call: &ToolCall) -> Vec<DisplayBlock> {
        let summary = match Self::action(call) {
            Some("run_blocking" | "run_background") => return Vec::new(),
            Some("log_file_path") => "Reading session log path",
            Some("send") => "Sending input",
            Some("view") => "Reading session output",
            Some("wait") => "Waiting for session",
            Some("terminate") => "Terminating session",
            _ => {
                return vec![value_to_display_block(&call.arguments, "No Bash arguments")];
            }
        };

        vec![DisplayBlock::Summary(summary.to_owned())]
    }

    fn terminal_block(output: &str) -> Option<DisplayBlock> {
        let output = strip_ansi_escapes::strip(output.as_bytes());
        let output = String::from_utf8_lossy(&output)
            .replace("\r\n", "\n")
            .replace('\r', "\n");
        let output = output.trim_end_matches('\n');

        if output.is_empty() {
            return None;
        }

        let (content, _) = truncate_preview(output);
        let truncated_lines = content.lines().count().max(1);

        Some(DisplayBlock::CodeBlock {
            language: Some("console".to_owned()),
            content,
            truncated_lines,
            show_line_numbers: false,
            start_line_number: 1,
        })
    }

    fn command_blocks(stdout: &str, stderr: &str) -> Vec<DisplayBlock> {
        let (stdout, stderr) = (Self::terminal_block(stdout), Self::terminal_block(stderr));

        if stdout.is_none() && stderr.is_none() {
            return vec![DisplayBlock::Summary(
                "Command completed with no output".to_owned(),
            )];
        }

        let mut blocks = vec![DisplayBlock::Summary("Command completed".to_owned())];
        if let Some(stdout) = stdout {
            blocks.push(DisplayBlock::Summary("stdout".to_owned()));
            blocks.push(stdout);
        }

        if let Some(stderr) = stderr {
            blocks.push(DisplayBlock::Summary("stderr".to_owned()));
            blocks.push(stderr);
        }

        blocks
    }

    fn session_output_blocks(call: &ToolCall, output: &str) -> Vec<DisplayBlock> {
        let (summary, empty_summary) = match Self::action(call) {
            Some("wait") => ("Session finished", "Session finished with no output"),
            Some("view") => ("Read session output", "No new session output"),
            _ => ("Received session output", "No session output"),
        };

        let Some(output) = Self::terminal_block(output) else {
            return vec![DisplayBlock::Summary(empty_summary.to_owned())];
        };

        vec![DisplayBlock::Summary(summary.to_owned()), output]
    }

    fn failed(call: &ToolCall, message: &str) -> Presentation {
        let message = truncate_chars(message, MAX_ERROR_CHARS);

        Self::presentation(
            call,
            ToolCallStatus::Failed {
                message: message.clone(),
            },
            vec![DisplayBlock::Summary(message)],
        )
    }

    fn succeeded(call: &ToolCall, output: &Value) -> Presentation {
        let Ok(output) = serde_json::from_value::<BashToolOutput>(output.clone()) else {
            return Self::presentation(
                call,
                ToolCallStatus::Succeeded,
                vec![value_to_display_block(output, "Completed")],
            );
        };

        match output {
            BashToolOutput::FilePath { path } => Self::presentation(
                call,
                ToolCallStatus::Succeeded,
                vec![
                    DisplayBlock::Summary("Session log path".to_owned()),
                    DisplayBlock::KeyValue {
                        entries: vec![KeyValueEntry {
                            key: "path".to_owned(),
                            value: path,
                        }],
                    },
                ],
            ),
            BashToolOutput::Sent => Self::presentation(
                call,
                ToolCallStatus::Succeeded,
                vec![DisplayBlock::Summary("Input sent".to_owned())],
            ),
            BashToolOutput::RanBlocking { stdout, stderr } => Self::presentation(
                call,
                ToolCallStatus::Succeeded,
                Self::command_blocks(&stdout, &stderr),
            ),
            BashToolOutput::Spawned { session_id } => Self::presentation(
                call,
                ToolCallStatus::Succeeded,
                vec![
                    DisplayBlock::Summary("Started background session".to_owned()),
                    DisplayBlock::KeyValue {
                        entries: vec![KeyValueEntry {
                            key: "session_id".to_owned(),
                            value: session_id,
                        }],
                    },
                ],
            ),
            BashToolOutput::Output { output } => Self::presentation(
                call,
                ToolCallStatus::Succeeded,
                Self::session_output_blocks(call, &output),
            ),
            BashToolOutput::NoBusyCommand => {
                Self::failed(call, "No command is running in this session")
            }
            BashToolOutput::Terminated => Self::presentation(
                call,
                ToolCallStatus::Succeeded,
                vec![DisplayBlock::Summary("Session terminated".to_owned())],
            ),
            BashToolOutput::SessionBusy => Self::failed(call, "Session is busy"),
            BashToolOutput::SessionNotExist => Self::failed(call, "Session does not exist"),
        }
    }
}

impl Presenter for BashPresenter {
    fn running(&self, call: &ToolCall) -> Presentation {
        Self::presentation(call, ToolCallStatus::Running, Self::running_blocks(call))
    }

    fn completed(&self, call: &ToolCall, result: &ToolCallResult) -> Presentation {
        match &result.outcome {
            ToolCallOutcome::Success(output) => Self::succeeded(call, output),
            ToolCallOutcome::Failure { message } => Self::failed(call, message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_preview_ends_on_a_unicode_boundary() {
        let output = "界".repeat(200);
        let end = preview_end(&output);

        assert!(end <= MAX_INLINE_OUTPUT_BYTES);
        assert!(output.is_char_boundary(end));
        assert_eq!(end, 510);
    }
}
