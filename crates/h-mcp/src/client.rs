use std::process::Stdio as ProcessStdio;

use anyhow::{Context, Result, bail};
use rmcp::{
    RoleClient, ServiceExt, model::CallToolRequestParams, service::RunningService,
    transport::TokioChildProcess,
};
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    task::JoinHandle,
};

use crate::{Output, Stdio, Tool};

pub struct Client {
    service: RunningService<RoleClient, ()>,
    stderr: Option<JoinHandle<std::io::Result<()>>>,
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

        Ok(Self { service, stderr })
    }

    pub async fn tools(&self) -> Result<Vec<Tool>> {
        let tools = self
            .service
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

        let result = self
            .service
            .call_tool(request)
            .await
            .with_context(|| format!("failed to call MCP tool `{name}`"))?;

        Output::try_from(result)
    }

    pub fn is_closed(&self) -> bool {
        self.service.is_closed()
    }

    pub async fn close(&mut self) -> Result<()> {
        self.service
            .close()
            .await
            .context("failed to close MCP client service")?;

        if let Some(stderr) = self.stderr.take() {
            stderr
                .await
                .context("failed to join MCP stderr task")?
                .context("failed to read MCP server stderr")?;
        }

        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn from_service(service: RunningService<RoleClient, ()>) -> Self {
        Self {
            service,
            stderr: None,
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
