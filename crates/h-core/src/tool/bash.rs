use std::{
    env,
    ffi::OsStr,
    io::{self, Write},
    os::unix::{fs::PermissionsExt, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::{Command as StdCommand, ExitStatus, Output, Stdio},
    sync::Arc,
    time::Duration,
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
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom},
    net::unix::pipe,
    process::Command,
    sync::Mutex as AsyncMutex,
    time::{sleep, timeout},
};
use uuid::Uuid;

use super::{
    DisplayBlock, KeyValueEntry, Presentation, Presenter, ToolCall, ToolCallOutcome,
    ToolCallResult, ToolCallStatus, ToolOutput, TypedTool,
    output::{Limits, save, save_and_preview},
    presentation::{MAX_ERROR_CHARS, MAX_FIELD_CHARS, truncate_chars, value_to_display_block},
};

const TMUX_HISTORY_LIMIT: &str = "100000";
const TMUX_POLL_INTERVAL: Duration = Duration::from_millis(50);
const TMUX_TERMINATE_TIMEOUT: Duration = Duration::from_secs(1);
const PREVIEW_EDGE_LINES: usize = 2;

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

struct PtyBashTool {
    sessions: DashMap<String, BashSession>,
}

impl PtyBashTool {
    fn new() -> Self {
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
            ViewResult::Ok { output } => Ok(BashToolOutput::Output { output, path: None }),
        }
    }

    async fn wait(&self, session_id: String) -> anyhow::Result<BashToolOutput> {
        let mut session = match self.get_mut(&session_id).await {
            Some(session) => session,
            None => return Ok(BashToolOutput::SessionNotExist),
        };
        match session.wait().await? {
            WaitResult::NoBusyCommand => Ok(BashToolOutput::NoBusyCommand),
            WaitResult::Ok { output } => Ok(BashToolOutput::Output { output, path: None }),
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

    async fn call(&self, args: BashToolArgs) -> anyhow::Result<BashToolOutput> {
        match args {
            BashToolArgs::RunBackground {
                command,
                session_id,
            } => self.run_background(session_id, command).await,
            BashToolArgs::LogFilePath { session_id } => Ok(self.log_path(session_id).await),
            BashToolArgs::Send { session_id, input } => self.send(session_id, input).await,
            BashToolArgs::View { session_id } => self.view(session_id).await,
            BashToolArgs::Wait { session_id } => self.wait(session_id).await,
            BashToolArgs::Terminate { session_id } => self.terminate(session_id).await,
            BashToolArgs::RunBlocking { .. } => unreachable!("blocking calls bypass the backend"),
        }
    }

    async fn cancel(&self, args: BashToolArgs) -> anyhow::Result<()> {
        let session_id = match args {
            BashToolArgs::RunBackground {
                session_id: Some(session_id),
                ..
            }
            | BashToolArgs::Wait { session_id } => Some(session_id),
            _ => None,
        };

        if let Some(session_id) = session_id {
            let _ = self.terminate(session_id).await?;
        }

        Ok(())
    }
}

struct TmuxSession {
    target: String,
    channel: String,
    worker_file: PathBuf,
    command_file: PathBuf,
    marker_file: PathBuf,
    history_file: PathBuf,
    offset: u64,
    marker: Option<String>,
    active: bool,
}

struct TmuxBashTool {
    executable: PathBuf,
    socket: String,
    socket_path: Mutex<Option<PathBuf>>,
    sessions: DashMap<String, Arc<AsyncMutex<TmuxSession>>>,
    /// A newly allocated session is recorded before tmux starts it. If the
    /// call future is dropped, the cancellation hook can still remove it.
    pending: Mutex<Option<String>>,
}

impl TmuxBashTool {
    fn available() -> bool {
        Self::find_executable().is_some_and(|executable| {
            StdCommand::new(executable)
                .arg("-V")
                .env_remove("TMUX")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        })
    }

    fn find_executable() -> Option<PathBuf> {
        let path = env::var_os("PATH")?;

        env::split_paths(&path).find_map(|directory| {
            let executable = directory.join("tmux");
            let metadata = executable.metadata().ok()?;

            if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
                return None;
            }

            executable.canonicalize().ok()
        })
    }

    fn new() -> Self {
        Self::with_socket(format!(
            "h-bash-{}-{}",
            std::process::id(),
            Uuid::new_v4().simple()
        ))
    }

    fn with_socket(socket: String) -> Self {
        Self {
            executable: Self::find_executable().unwrap_or_else(|| PathBuf::from("tmux")),
            socket,
            socket_path: Mutex::new(None),
            sessions: DashMap::new(),
            pending: Mutex::new(None),
        }
    }

    fn session(&self, session_id: &str) -> Option<Arc<AsyncMutex<TmuxSession>>> {
        self.sessions
            .get(session_id)
            .map(|session| Arc::clone(session.value()))
    }

    async fn tmux<I, S>(&self, args: I) -> anyhow::Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = Command::new(&self.executable)
            .arg("-L")
            .arg(&self.socket)
            .args(args)
            .env_remove("TMUX")
            .output()
            .await?;

        if output.status.success() {
            return Ok(output);
        }

        let error = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("tmux command failed: {}", error.trim());
    }

    async fn has_session(&self, target: &str) -> anyhow::Result<bool> {
        let status = Command::new(&self.executable)
            .arg("-L")
            .arg(&self.socket)
            .args(["has-session", "-t", target])
            .env_remove("TMUX")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await?;

        Ok(status.success())
    }

    async fn pane_dead(&self, target: &str) -> anyhow::Result<bool> {
        let output = self
            .tmux(["display-message", "-p", "-t", target, "#{pane_dead}"])
            .await?;

        match String::from_utf8_lossy(&output.stdout).trim() {
            "0" => Ok(false),
            "1" => Ok(true),
            value => anyhow::bail!("tmux returned an invalid pane state: {value}"),
        }
    }

    fn shell_quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\\''"))
    }

    fn path_string(path: &Path) -> anyhow::Result<&str> {
        path.to_str()
            .ok_or_else(|| anyhow::anyhow!("tmux does not support a non-UTF-8 temporary path"))
    }

    async fn write_file(path: &Path, content: &[u8]) -> anyhow::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .await?;

        file.write_all(content).await?;
        file.flush().await?;
        Ok(())
    }

    async fn create_session(&self, session: &TmuxSession) -> anyhow::Result<()> {
        let (executable, command_file, marker_file) = (
            Self::shell_quote(Self::path_string(&self.executable)?),
            Self::shell_quote(Self::path_string(&session.command_file)?),
            Self::shell_quote(Self::path_string(&session.marker_file)?),
        );
        let (socket, channel, target) = (
            Self::shell_quote(&self.socket),
            Self::shell_quote(&session.channel),
            Self::shell_quote(&session.target),
        );
        let worker = format!(
            "shopt -s expand_aliases\n\
             while {executable} -L {socket} wait-for {channel}; do\n\
                 builtin source {command_file} || true\n\
                 {executable} -L {socket} set-option -p -t {target} @h_done \"$(< {marker_file})\"\n\
             done\n"
        );
        Self::write_file(&session.worker_file, worker.as_bytes()).await?;
        Self::write_file(&session.command_file, &[]).await?;
        Self::write_file(&session.marker_file, &[]).await?;
        Self::write_file(&session.history_file, &[]).await?;

        let (cwd, worker_file) = (
            env::current_dir()?,
            Self::shell_quote(Self::path_string(&session.worker_file)?),
        );
        let bootstrap = format!("exec bash --noprofile --norc {worker_file}");
        self.tmux([
            OsStr::new("new-session"),
            OsStr::new("-d"),
            OsStr::new("-s"),
            OsStr::new(&session.target),
            OsStr::new("-c"),
            cwd.as_os_str(),
            OsStr::new(&bootstrap),
        ])
        .await?;

        let socket_path = self
            .tmux(["display-message", "-p", "#{socket_path}"])
            .await?;
        let socket_path = String::from_utf8_lossy(&socket_path.stdout)
            .trim()
            .to_owned();
        *self.socket_path.lock() = Some(PathBuf::from(socket_path));

        let history_file = Self::shell_quote(Self::path_string(&session.history_file)?);
        let pipe = format!("cat >> {history_file}");
        let setup = async {
            self.tmux([
                "set-option",
                "-w",
                "-t",
                &session.target,
                "remain-on-exit",
                "on",
            ])
            .await?;
            self.tmux([
                "set-option",
                "-w",
                "-t",
                &session.target,
                "history-limit",
                TMUX_HISTORY_LIMIT,
            ])
            .await?;
            self.tmux(["pipe-pane", "-t", &session.target, &pipe])
                .await?;

            Ok::<_, anyhow::Error>(())
        }
        .await;

        if let Err(error) = setup {
            let _ = self.tmux(["kill-session", "-t", &session.target]).await;
            return Err(error);
        }

        Ok(())
    }

    async fn start(&self, session: &mut TmuxSession, command: &str) -> anyhow::Result<()> {
        let marker = Uuid::new_v4().simple().to_string();
        Self::write_file(&session.command_file, command.as_bytes()).await?;
        Self::write_file(&session.marker_file, marker.as_bytes()).await?;

        let history_len = fs::metadata(&session.history_file).await?.len();
        self.tmux(["set-option", "-p", "-t", &session.target, "@h_done", ""])
            .await?;

        session.offset = history_len;
        session.marker = Some(marker);
        session.active = true;

        if let Err(error) = self.tmux(["wait-for", "-S", &session.channel]).await {
            session.marker = None;
            session.active = false;
            return Err(error);
        }

        Ok(())
    }

    async fn command_done(&self, session: &TmuxSession) -> anyhow::Result<bool> {
        let Some(marker) = &session.marker else {
            return Ok(!session.active);
        };

        if !self.has_session(&session.target).await? || self.pane_dead(&session.target).await? {
            return Ok(true);
        }

        let output = self
            .tmux(["show-options", "-p", "-v", "-t", &session.target, "@h_done"])
            .await?;

        Ok(String::from_utf8_lossy(&output.stdout).trim() == marker)
    }

    async fn wait_for_completion(&self, session: &TmuxSession) -> anyhow::Result<()> {
        while !self.command_done(session).await? {
            sleep(TMUX_POLL_INTERVAL).await;
        }

        // Completion is signalled from the pane after the command returns.
        // Give pipe-pane one scheduling interval to flush the preceding bytes.
        sleep(TMUX_POLL_INTERVAL).await;
        Ok(())
    }

    async fn read_new(session: &mut TmuxSession) -> anyhow::Result<String> {
        let mut file = OpenOptions::new()
            .read(true)
            .open(&session.history_file)
            .await?;

        file.seek(SeekFrom::Start(session.offset)).await?;

        let mut content = Vec::new();
        file.read_to_end(&mut content).await?;
        session.offset = session.offset.saturating_add(content.len() as u64);

        let content = strip_ansi_escapes::strip(content);
        Ok(String::from_utf8_lossy(&content).into_owned())
    }

    async fn run_background(
        &self,
        session_id: Option<String>,
        command: String,
    ) -> anyhow::Result<BashToolOutput> {
        if let Some(session_id) = session_id {
            let Some(session) = self.session(&session_id) else {
                return Ok(BashToolOutput::SessionNotExist);
            };
            let mut session = session.lock().await;

            if !self.has_session(&session.target).await? || self.pane_dead(&session.target).await? {
                return Ok(BashToolOutput::SessionNotExist);
            }

            if session.active {
                if !self.command_done(&session).await? {
                    return Ok(BashToolOutput::SessionBusy);
                }

                self.wait_for_completion(&session).await?;
                session.active = false;
                session.marker = None;
            }

            if self.pane_dead(&session.target).await? {
                return Ok(BashToolOutput::SessionNotExist);
            }

            self.start(&mut session, &command).await?;
            return Ok(BashToolOutput::Spawned { session_id });
        }

        let session_id = Uuid::new_v4().to_string();
        let file_prefix = env::temp_dir().join(format!("h_tmux_{session_id}"));
        let session = Arc::new(AsyncMutex::new(TmuxSession {
            target: format!("h-{session_id}"),
            channel: format!("h-command-{session_id}"),
            worker_file: file_prefix.with_extension("worker.sh"),
            command_file: file_prefix.with_extension("command.sh"),
            marker_file: file_prefix.with_extension("marker"),
            history_file: file_prefix.with_extension("log"),
            offset: 0,
            marker: None,
            active: false,
        }));

        self.sessions
            .insert(session_id.clone(), Arc::clone(&session));
        *self.pending.lock() = Some(session_id.clone());

        let result = {
            let mut session = session.lock().await;

            match self.create_session(&session).await {
                Ok(()) => self.start(&mut session, &command).await,
                Err(error) => Err(error),
            }
        };

        {
            let mut pending = self.pending.lock();
            if pending.as_deref() == Some(&session_id) {
                pending.take();
            }
        }

        if let Err(error) = result {
            let session = session.lock().await;
            if self.has_session(&session.target).await.unwrap_or(false) {
                let _ = self.tmux(["kill-session", "-t", &session.target]).await;
            }
            Self::cleanup_files(&session).await;
            drop(session);

            self.sessions.remove(&session_id);
            return Err(error);
        }

        Ok(BashToolOutput::Spawned { session_id })
    }

    async fn view(&self, session_id: String) -> anyhow::Result<BashToolOutput> {
        let Some(session) = self.session(&session_id) else {
            return Ok(BashToolOutput::SessionNotExist);
        };
        let mut session = session.lock().await;

        if !session.active {
            return Ok(BashToolOutput::NoBusyCommand);
        }

        let output = Self::read_new(&mut session).await?;
        Ok(BashToolOutput::Output { output, path: None })
    }

    async fn wait(&self, session_id: String) -> anyhow::Result<BashToolOutput> {
        let Some(session) = self.session(&session_id) else {
            return Ok(BashToolOutput::SessionNotExist);
        };
        let mut session = session.lock().await;

        if !session.active {
            return Ok(BashToolOutput::NoBusyCommand);
        }

        self.wait_for_completion(&session).await?;

        let output = Self::read_new(&mut session).await?;
        session.active = false;
        session.marker = None;
        Ok(BashToolOutput::Output { output, path: None })
    }

    async fn terminate(&self, session_id: String) -> anyhow::Result<BashToolOutput> {
        let Some(session) = self.session(&session_id) else {
            return Ok(BashToolOutput::SessionNotExist);
        };
        let mut session = session.lock().await;

        if !session.active || self.command_done(&session).await? {
            session.active = false;
            session.marker = None;
            return Ok(BashToolOutput::NoBusyCommand);
        }

        self.tmux(["send-keys", "-t", &session.target, "C-c"])
            .await?;

        let completed = timeout(TMUX_TERMINATE_TIMEOUT, self.wait_for_completion(&session)).await;
        if completed.is_err() {
            self.tmux(["kill-session", "-t", &session.target]).await?;
        } else {
            completed??;
        }

        session.active = false;
        session.marker = None;
        Ok(BashToolOutput::Terminated)
    }

    async fn log_path(&self, session_id: String) -> BashToolOutput {
        let Some(session) = self.session(&session_id) else {
            return BashToolOutput::SessionNotExist;
        };
        let session = session.lock().await;

        BashToolOutput::FilePath {
            path: session.history_file.display().to_string(),
        }
    }

    async fn send(&self, session_id: String, input: String) -> anyhow::Result<BashToolOutput> {
        let Some(session) = self.session(&session_id) else {
            return Ok(BashToolOutput::SessionNotExist);
        };
        let session = session.lock().await;

        if !session.active || self.command_done(&session).await? {
            return Ok(BashToolOutput::NoBusyCommand);
        }

        let buffer = format!("h-input-{}", Uuid::new_v4().simple());
        let mut process = Command::new(&self.executable)
            .arg("-L")
            .arg(&self.socket)
            .args(["load-buffer", "-b", &buffer, "-"])
            .env_remove("TMUX")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdin = process
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("tmux input pipe was not created"))?;

        stdin.write_all(input.as_bytes()).await?;
        drop(stdin);

        let output = process.wait_with_output().await?;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("tmux command failed: {}", error.trim());
        }

        self.tmux(["paste-buffer", "-d", "-b", &buffer, "-t", &session.target])
            .await?;
        Ok(BashToolOutput::Sent)
    }

    async fn cleanup_files(session: &TmuxSession) {
        for path in [
            &session.worker_file,
            &session.command_file,
            &session.marker_file,
            &session.history_file,
        ] {
            let _ = fs::remove_file(path).await;
        }
    }

    async fn call(&self, args: BashToolArgs) -> anyhow::Result<BashToolOutput> {
        match args {
            BashToolArgs::RunBackground {
                command,
                session_id,
            } => self.run_background(session_id, command).await,
            BashToolArgs::LogFilePath { session_id } => Ok(self.log_path(session_id).await),
            BashToolArgs::Send { session_id, input } => self.send(session_id, input).await,
            BashToolArgs::View { session_id } => self.view(session_id).await,
            BashToolArgs::Wait { session_id } => self.wait(session_id).await,
            BashToolArgs::Terminate { session_id } => self.terminate(session_id).await,
            BashToolArgs::RunBlocking { .. } => unreachable!("blocking calls bypass the backend"),
        }
    }

    async fn cancel(&self, args: BashToolArgs) -> anyhow::Result<()> {
        let (session_id, was_pending) = match args {
            BashToolArgs::RunBackground {
                session_id: Some(session_id),
                ..
            }
            | BashToolArgs::Wait { session_id } => (Some(session_id), false),
            BashToolArgs::RunBackground {
                session_id: None, ..
            } => (self.pending.lock().take(), true),
            _ => (None, false),
        };

        if let Some(session_id) = session_id {
            if was_pending && let Some((_, session)) = self.sessions.remove(&session_id) {
                let session = session.lock().await;
                if self.has_session(&session.target).await? {
                    let _ = self.tmux(["kill-session", "-t", &session.target]).await;
                }
                Self::cleanup_files(&session).await;
            } else {
                let _ = self.terminate(session_id).await?;
            }
        }

        Ok(())
    }
}

impl Drop for TmuxBashTool {
    fn drop(&mut self) {
        let _ = StdCommand::new(&self.executable)
            .arg("-L")
            .arg(&self.socket)
            .arg("kill-server")
            .env_remove("TMUX")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();

        if let Some(socket_path) = self.socket_path.get_mut().take() {
            let _ = std::fs::remove_file(socket_path);
        }

        for session in &self.sessions {
            if let Ok(session) = session.value().try_lock() {
                for path in [
                    &session.worker_file,
                    &session.command_file,
                    &session.marker_file,
                    &session.history_file,
                ] {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
    }
}

enum BashBackend {
    Pty(PtyBashTool),
    Tmux(TmuxBashTool),
}

pub struct BashTool {
    backend: BashBackend,
}

impl BashTool {
    pub fn new() -> Self {
        Self::with_tmux_available(TmuxBashTool::available())
    }

    fn with_tmux_available(available: bool) -> Self {
        let backend = if available {
            BashBackend::Tmux(TmuxBashTool::new())
        } else {
            BashBackend::Pty(PtyBashTool::new())
        };
        let name = match &backend {
            BashBackend::Pty(_) => "pty",
            BashBackend::Tmux(_) => "tmux",
        };

        tracing::info!(event = "tool.bash.backend.selected", backend = name);
        Self { backend }
    }

    async fn call_backend(&self, args: BashToolArgs) -> anyhow::Result<BashToolOutput> {
        match &self.backend {
            BashBackend::Pty(tool) => tool.call(args).await,
            BashBackend::Tmux(tool) => tool.call(args).await,
        }
    }

    async fn cancel_backend(&self, args: BashToolArgs) -> anyhow::Result<()> {
        match &self.backend {
            BashBackend::Pty(tool) => tool.cancel(args).await,
            BashBackend::Tmux(tool) => tool.cancel(args).await,
        }
    }
}

impl Default for BashTool {
    fn default() -> Self {
        Self::new()
    }
}

async fn run_blocking(command: String, brief: bool) -> anyhow::Result<BashToolOutput> {
    if command.split_whitespace().next().is_none() {
        anyhow::bail!("Empty command");
    }

    let (status, output) = capture_output(&command).await?;
    let exit_code = status.code();
    let signal = status.signal();

    if brief && status.success() {
        let output_path = save(&output, "bash-output").await?;
        let command = truncate_chars(&command, MAX_FIELD_CHARS);

        return Ok(BashToolOutput::Succeeded {
            summary: format!("Command {command:?} succeeded."),
            exit_code: 0,
            output_path,
        });
    }

    let output = save_and_preview(&output, "bash-output", Limits::DEFAULT).await?;

    Ok(BashToolOutput::RanBlocking {
        output: output.content,
        exit_code,
        signal,
    })
}

async fn capture_output(command: &str) -> anyhow::Result<(ExitStatus, String)> {
    let (writer, mut reader) = pipe::pipe()?;
    let writer = writer.into_blocking_fd()?;
    let stderr = writer.try_clone()?;

    let mut process = Command::new("bash");
    process
        .arg("-c")
        .arg(command)
        .stdout(Stdio::from(writer))
        .stderr(Stdio::from(stderr))
        .kill_on_drop(true);

    let mut child = process.spawn()?;
    drop(process);

    let mut output = Vec::new();
    let (status, _) = tokio::try_join!(child.wait(), reader.read_to_end(&mut output))?;

    Ok((status, String::from_utf8_lossy(&output).into_owned()))
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum BashToolArgs {
    /// Run a command and block until it completes.
    RunBlocking {
        /// The shell command to execute.
        command: String,
        /// Return only a short summary when the command exits successfully. Failed commands always include their output. Defaults to false.
        brief: Option<bool>,
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
        "Execute bash commands and manage background terminal sessions; run_blocking can set brief=true to suppress successful output while preserving it in temporary files"
    }

    async fn call(&self, args: Self::Arguments) -> anyhow::Result<ToolOutput<Self::Output>> {
        let output = match args {
            BashToolArgs::RunBlocking { command, brief } => {
                run_blocking(command, brief.unwrap_or(false)).await
            }
            args => self.call_backend(args).await,
        }?;
        let output = output.limit().await?;

        Ok(ToolOutput::new(output))
    }

    async fn cancel(&self, args: Self::Arguments) -> anyhow::Result<()> {
        if matches!(&args, BashToolArgs::RunBlocking { .. }) {
            // A blocking command is owned by its call future and configured
            // with `kill_on_drop`, so dropping that future is sufficient.
            return Ok(());
        }

        self.cancel_backend(args).await
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum BashToolOutput {
    FilePath {
        path: String,
    },
    Sent,
    Succeeded {
        summary: String,
        exit_code: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        output_path: Option<String>,
    },
    RanBlocking {
        output: String,
        #[serde(default)]
        exit_code: Option<i32>,
        #[serde(default)]
        signal: Option<i32>,
    },
    Spawned {
        session_id: String,
    },
    Output {
        output: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    NoBusyCommand,
    Terminated,
    SessionBusy,
    SessionNotExist,
}

impl BashToolOutput {
    async fn limit(self) -> anyhow::Result<Self> {
        let Self::Output { output, .. } = self else {
            return Ok(self);
        };
        let preview = save_and_preview(&output, "bash-session", Limits::DEFAULT).await?;

        Ok(Self::Output {
            output: preview.content,
            path: preview.path,
        })
    }
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

        let lines = output.lines().collect::<Vec<_>>();
        let content = if lines.len() <= PREVIEW_EDGE_LINES * 2 {
            lines.join("\n")
        } else {
            let omitted = lines.len() - PREVIEW_EDGE_LINES * 2;
            let (head, tail) = lines.split_at(PREVIEW_EDGE_LINES);
            let tail = &tail[tail.len() - PREVIEW_EDGE_LINES..];

            format!(
                "{}\n... +{omitted} lines\n{}",
                head.join("\n"),
                tail.join("\n")
            )
        };
        let visible_lines = content.lines().count().max(1);

        Some(DisplayBlock::CodeBlock {
            language: Some("console".to_owned()),
            content,
            truncated_lines: visible_lines,
            show_line_numbers: false,
            start_line_number: 1,
        })
    }

    fn command_blocks(output: &str, signal: Option<i32>) -> Vec<DisplayBlock> {
        let output = Self::terminal_block(output);
        let summary = if output.is_none() {
            "Command completed with no output"
        } else {
            "Command completed"
        };
        let mut blocks = vec![DisplayBlock::Summary(summary.to_owned())];
        let mut status = Vec::new();

        if let Some(signal) = signal {
            status.push(KeyValueEntry {
                key: "signal".to_owned(),
                value: signal.to_string(),
            });
        }
        if !status.is_empty() {
            blocks.push(DisplayBlock::KeyValue { entries: status });
        }

        if let Some(output) = output {
            blocks.push(output);
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
            BashToolOutput::Succeeded {
                summary,
                output_path,
                ..
            } => {
                let mut blocks = vec![DisplayBlock::Summary(summary)];

                if let Some(path) = output_path {
                    blocks.push(DisplayBlock::KeyValue {
                        entries: vec![KeyValueEntry {
                            key: "output_path".to_owned(),
                            value: path,
                        }],
                    });
                }

                Self::presentation(call, ToolCallStatus::Succeeded, blocks)
            }
            BashToolOutput::RanBlocking { output, signal, .. } => Self::presentation(
                call,
                ToolCallStatus::Succeeded,
                Self::command_blocks(&output, signal),
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
            BashToolOutput::Output { output, .. } => Self::presentation(
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

    fn tmux_tool() -> Option<TmuxBashTool> {
        TmuxBashTool::available()
            .then(|| TmuxBashTool::with_socket(format!("h-bash-test-{}", Uuid::new_v4().simple())))
    }

    fn spawned(output: BashToolOutput) -> String {
        match output {
            BashToolOutput::Spawned { session_id } => session_id,
            output => panic!("expected a spawned session, got {output:?}"),
        }
    }

    fn output(output: BashToolOutput) -> String {
        match output {
            BashToolOutput::Output { output, .. } => output,
            output => panic!("expected session output, got {output:?}"),
        }
    }

    async fn wait_until_done(tool: &TmuxBashTool, session_id: &str) {
        let session = tool.session(session_id).unwrap();

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let session = session.lock().await;
                if tool.command_done(&session).await.unwrap() {
                    break;
                }
                drop(session);

                sleep(TMUX_POLL_INTERVAL).await;
            }
        })
        .await
        .expect("the tmux command should finish");
    }

    #[tokio::test]
    async fn brief_success_keeps_output_in_file() {
        let result = run_blocking("printf stdout; printf stderr >&2".to_owned(), true)
            .await
            .unwrap();
        let BashToolOutput::Succeeded {
            summary,
            output_path: Some(output_path),
            ..
        } = result
        else {
            panic!("expected a brief success result");
        };

        assert_eq!(
            summary,
            "Command \"printf stdout; printf stderr >&2\" succeeded."
        );
        assert_eq!(
            fs::read_to_string(&output_path).await.unwrap(),
            "stdoutstderr"
        );

        fs::remove_file(output_path).await.unwrap();
    }

    #[tokio::test]
    async fn blocking_preserves_output_order() {
        let command = "printf out1; printf err1 >&2; printf out2; printf err2 >&2";
        let result = run_blocking(command.to_owned(), false).await.unwrap();

        assert!(matches!(
            result,
            BashToolOutput::RanBlocking {
                ref output,
                exit_code: Some(0),
                signal: None,
            } if output == "out1err1out2err2"
        ));
    }

    #[tokio::test]
    async fn brief_does_not_hide_failed_output() {
        let result = run_blocking("printf failure; exit 7".to_owned(), true)
            .await
            .unwrap();

        assert!(matches!(
            result,
            BashToolOutput::RanBlocking {
                ref output,
                exit_code: Some(7),
                ..
            } if output == "failure"
        ));
    }

    #[tokio::test]
    async fn non_brief_success_saves_full_output_when_the_preview_is_truncated() {
        let command = "i=0; while [ \"$i\" -lt 3000 ]; do printf x; i=$((i + 1)); done";
        let result = run_blocking(command.to_owned(), false).await.unwrap();
        let BashToolOutput::RanBlocking {
            output,
            exit_code: Some(0),
            signal: None,
        } = result
        else {
            panic!("expected a successful blocking result");
        };
        let output_path = output
            .lines()
            .find_map(|line| line.strip_prefix("Full output: "))
            .unwrap();

        assert!(output.contains("bytes omitted"));
        assert_eq!(
            fs::read_to_string(output_path).await.unwrap(),
            "x".repeat(3_000)
        );

        fs::remove_file(output_path).await.unwrap();
    }

    #[tokio::test]
    async fn blocking_reports_a_nonzero_exit_code() {
        let result = run_blocking("exit 7".to_owned(), false).await.unwrap();

        assert!(matches!(
            result,
            BashToolOutput::RanBlocking {
                ref output,
                exit_code: Some(7),
                signal: None,
            } if output.is_empty()
        ));
        assert_eq!(
            serde_json::to_value(result).unwrap(),
            serde_json::json!({
                "RanBlocking": {
                    "output": "",
                    "exit_code": 7,
                    "signal": null,
                }
            })
        );
    }

    #[tokio::test]
    async fn blocking_reports_the_terminating_signal() {
        let result = run_blocking("kill -TERM $$".to_owned(), false)
            .await
            .unwrap();

        assert!(matches!(
            result,
            BashToolOutput::RanBlocking {
                ref output,
                exit_code: None,
                signal: Some(15),
            } if output.is_empty()
        ));
        assert_eq!(
            serde_json::to_value(result).unwrap(),
            serde_json::json!({
                "RanBlocking": {
                    "output": "",
                    "exit_code": null,
                    "signal": 15,
                }
            })
        );
    }

    #[test]
    fn bash_tool_selects_the_available_backend() {
        assert!(matches!(
            BashTool::with_tmux_available(true).backend,
            BashBackend::Tmux(_)
        ));
        assert!(matches!(
            BashTool::with_tmux_available(false).backend,
            BashBackend::Pty(_)
        ));
    }

    #[tokio::test]
    async fn tmux_backend_waits_for_output_and_reuses_a_finished_session() {
        let Some(tool) = tmux_tool() else {
            return;
        };

        let session_id = spawned(
            tool.run_background(None, "printf first".to_owned())
                .await
                .unwrap(),
        );
        let first = output(tool.wait(session_id.clone()).await.unwrap());
        assert_eq!(first, "first");

        let reused = spawned(
            tool.run_background(Some(session_id.clone()), "printf second".to_owned())
                .await
                .unwrap(),
        );
        assert_eq!(reused, session_id);

        let second = output(tool.wait(session_id).await.unwrap());
        assert_eq!(second, "second");
    }

    #[tokio::test]
    async fn tmux_backend_preserves_shell_state_between_commands() {
        let Some(tool) = tmux_tool() else {
            return;
        };

        let directory = env::temp_dir();
        let command = format!(
            "cd {}; export H_TMUX_TEST_STATE=preserved",
            TmuxBashTool::shell_quote(directory.to_str().unwrap())
        );
        let session_id = spawned(tool.run_background(None, command).await.unwrap());
        assert_eq!(output(tool.wait(session_id.clone()).await.unwrap()), "");

        let session_id = spawned(
            tool.run_background(
                Some(session_id),
                "printf '%s:%s' \"$PWD\" \"$H_TMUX_TEST_STATE\"".to_owned(),
            )
            .await
            .unwrap(),
        );
        let state = output(tool.wait(session_id).await.unwrap());

        assert_eq!(state, format!("{}:preserved", directory.display()));
    }

    #[tokio::test]
    async fn tmux_backend_keeps_historical_output_in_its_log() {
        let Some(tool) = tmux_tool() else {
            return;
        };

        let session_id = spawned(
            tool.run_background(None, "printf first".to_owned())
                .await
                .unwrap(),
        );
        tool.wait(session_id.clone()).await.unwrap();
        let session_id = spawned(
            tool.run_background(Some(session_id), "printf second".to_owned())
                .await
                .unwrap(),
        );
        tool.wait(session_id.clone()).await.unwrap();

        let BashToolOutput::FilePath { path } = tool.log_path(session_id).await else {
            panic!("expected a log file path");
        };
        let history = fs::read_to_string(path).await.unwrap();

        assert!(history.contains("first"));
        assert!(history.contains("second"));
    }

    #[tokio::test]
    async fn tmux_backend_notices_completion_without_an_explicit_wait() {
        let Some(tool) = tmux_tool() else {
            return;
        };

        let session_id = spawned(
            tool.run_background(None, "printf first".to_owned())
                .await
                .unwrap(),
        );
        wait_until_done(&tool, &session_id).await;

        let reused = tool
            .run_background(Some(session_id.clone()), "printf second".to_owned())
            .await
            .unwrap();
        assert_eq!(spawned(reused), session_id);
        assert_eq!(output(tool.wait(session_id).await.unwrap()), "second");
    }

    #[tokio::test]
    async fn bash_tool_dispatches_background_calls_to_tmux() {
        let Some(backend) = tmux_tool() else {
            return;
        };
        let tool = BashTool {
            backend: BashBackend::Tmux(backend),
        };

        let session_id = spawned(
            TypedTool::call(
                &tool,
                BashToolArgs::RunBackground {
                    command: "printf dispatched".to_owned(),
                    session_id: None,
                },
            )
            .await
            .unwrap()
            .into_value(),
        );
        let waited = TypedTool::call(&tool, BashToolArgs::Wait { session_id })
            .await
            .unwrap()
            .into_value();

        assert_eq!(output(waited), "dispatched");
    }

    #[tokio::test]
    async fn tmux_backend_sends_literal_input_to_a_running_command() {
        let Some(tool) = tmux_tool() else {
            return;
        };

        let session_id = spawned(
            tool.run_background(
                None,
                "read value; printf 'received:%s' \"$value\"".to_owned(),
            )
            .await
            .unwrap(),
        );

        tool.send(session_id.clone(), "hello world\n".to_owned())
            .await
            .unwrap();

        let waited = tokio::time::timeout(Duration::from_secs(2), tool.wait(session_id))
            .await
            .expect("the newline should submit the input")
            .unwrap();
        assert!(output(waited).contains("received:hello world"));
    }

    #[tokio::test]
    async fn tmux_backend_terminates_the_session() {
        let Some(tool) = tmux_tool() else {
            return;
        };

        let session_id = spawned(
            tool.run_background(None, "sleep 30".to_owned())
                .await
                .unwrap(),
        );

        assert!(matches!(
            tool.terminate(session_id.clone()).await.unwrap(),
            BashToolOutput::Terminated
        ));
        assert!(matches!(
            tool.view(session_id).await.unwrap(),
            BashToolOutput::NoBusyCommand
        ));
    }

    #[tokio::test]
    async fn tmux_backend_rejects_a_second_command_while_busy() {
        let Some(tool) = tmux_tool() else {
            return;
        };

        let session_id = spawned(
            tool.run_background(None, "sleep 30".to_owned())
                .await
                .unwrap(),
        );
        let second = tool
            .run_background(Some(session_id.clone()), "printf second".to_owned())
            .await
            .unwrap();

        assert!(matches!(second, BashToolOutput::SessionBusy));
        tool.terminate(session_id).await.unwrap();
    }

    #[tokio::test]
    async fn cancelling_a_tmux_wait_terminates_its_session() {
        let Some(tool) = tmux_tool() else {
            return;
        };

        let session_id = spawned(
            tool.run_background(None, "sleep 30".to_owned())
                .await
                .unwrap(),
        );

        tool.cancel(BashToolArgs::Wait {
            session_id: session_id.clone(),
        })
        .await
        .unwrap();

        assert!(matches!(
            tool.view(session_id).await.unwrap(),
            BashToolOutput::NoBusyCommand
        ));
    }
}
