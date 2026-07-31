use std::{
    process::Stdio as ProcessStdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context, Result, bail};
use rmcp::{
    RoleClient, ServiceExt, model::CallToolRequestParams, service::RunningService,
    transport::TokioChildProcess,
};
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::{Mutex, RwLock},
    task::JoinHandle,
};

use crate::{Output, Stdio, Tool};

#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

struct Inner {
    service: RwLock<RunningService<RoleClient, ()>>,
    stderr: Mutex<Option<JoinHandle<std::io::Result<()>>>>,
    close: Mutex<()>,
    closed: AtomicBool,
}

impl Client {
    pub async fn connect(config: Stdio) -> Result<Self> {
        let mut command = tokio::process::Command::new(&config.program);
        command.args(&config.args).envs(&config.env);

        if let Some(cwd) = &config.cwd {
            command.current_dir(cwd);
        }

        let program = config.program.to_string_lossy().into_owned();
        let (transport, stderr) = TokioChildProcess::builder(command)
            .stderr(ProcessStdio::piped())
            .spawn()
            .with_context(|| format!("failed to start MCP server `{program}`"))?;
        let stderr = stderr.map(drain_stderr);
        let service = ()
            .serve(transport)
            .await
            .with_context(|| format!("failed to initialize MCP server `{program}`"))?;

        Ok(Self::new(service, stderr))
    }

    pub async fn tools(&self) -> Result<Vec<Tool>> {
        let service = self.inner.service.read().await;
        let tools = service
            .list_all_tools()
            .await
            .context("failed to list MCP tools")?;
        let mut tools = tools
            .into_iter()
            .map(Tool::try_from)
            .collect::<Result<Vec<_>>>()?;
        tools.sort_by(|left, right| left.name.cmp(&right.name));

        Ok(tools)
    }

    pub async fn call(&self, name: &str, arguments: Value) -> Result<Output> {
        let arguments = match arguments {
            Value::Null => None,
            Value::Object(arguments) => Some(arguments),
            value => {
                bail!(
                    "MCP tool arguments must be a JSON object or null, got {}",
                    json_type(&value)
                );
            }
        };

        let mut request = CallToolRequestParams::new(name.to_owned());
        if let Some(arguments) = arguments {
            request = request.with_arguments(arguments);
        }

        let service = self.inner.service.read().await;
        let result = service
            .call_tool(request)
            .await
            .with_context(|| format!("failed to call MCP tool `{name}`"))?;

        Output::try_from(result)
    }

    pub fn is_closed(&self) -> bool {
        if self.inner.closed.load(Ordering::Acquire) {
            return true;
        }

        self.inner
            .service
            .try_read()
            .is_ok_and(|service| service.is_closed())
    }

    pub async fn close(&self) -> Result<()> {
        let _close = self.inner.close.lock().await;

        if self.inner.closed.load(Ordering::Acquire) {
            return Ok(());
        }

        self.inner
            .service
            .write()
            .await
            .close()
            .await
            .context("failed to close MCP client service")?;
        self.inner.closed.store(true, Ordering::Release);

        if let Some(stderr) = self.inner.stderr.lock().await.take() {
            stderr
                .await
                .context("failed to join MCP stderr task")?
                .context("failed to read MCP server stderr")?;
        }

        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn from_service(service: RunningService<RoleClient, ()>) -> Self {
        Self::new(service, None)
    }

    fn new(
        service: RunningService<RoleClient, ()>,
        stderr: Option<JoinHandle<std::io::Result<()>>>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                service: RwLock::new(service),
                stderr: Mutex::new(stderr),
                close: Mutex::new(()),
                closed: AtomicBool::new(false),
            }),
        }
    }
}

fn drain_stderr(stderr: tokio::process::ChildStderr) -> JoinHandle<std::io::Result<()>> {
    tokio::spawn(async move {
        let mut stderr = BufReader::new(stderr);
        let mut line = Vec::new();

        loop {
            line.clear();
            let read = stderr.read_until(b'\n', &mut line).await?;
            if read == 0 {
                break;
            }

            while matches!(line.last(), Some(b'\n' | b'\r')) {
                line.pop();
            }

            tracing::warn!(
                target: "h_mcp::server",
                message = %String::from_utf8_lossy(&line),
                "MCP server stderr"
            );
        }

        Ok(())
    })
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
