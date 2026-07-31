use std::collections::HashSet;

use anyhow::Context as _;
use h_core::{
    agent::Agent,
    provider::Provider,
    tool::{DynTool, Summary, ToolDefinition, ToolOutput},
};
use serde_json::Value;

use crate::{Client, Config, Server, Tool as RemoteTool};

struct Connection {
    id: String,
    client: Client,
    tools: Vec<RemoteTool>,
}

pub struct Runtime {
    connections: Vec<Connection>,
}

impl Runtime {
    pub async fn start(config: &Config) -> anyhow::Result<Self> {
        config.validate().context("invalid MCP configuration")?;

        let mut connections = Vec::new();

        for (id, server) in config.servers() {
            if !server.enabled() {
                tracing::debug!(event = "mcp.server.skipped", server_id = id);
                continue;
            }

            match Connection::start(id, server).await {
                Ok(connection) => connections.push(connection),
                Err(error) => {
                    close_connections(&connections).await;
                    return Err(error);
                }
            }
        }

        Ok(Self { connections })
    }

    pub fn register<P>(&self, agent: &mut Agent<P>) -> anyhow::Result<usize>
    where
        P: Provider,
    {
        let mut count = 0;

        for connection in &self.connections {
            for tool in &connection.tools {
                agent.register_tool(Tool::new(connection, tool)?);
                count += 1;
            }
        }

        Ok(count)
    }

    pub fn server_count(&self) -> usize {
        self.connections.len()
    }

    pub async fn close(self) -> anyhow::Result<()> {
        let mut first_error = None;

        for connection in self.connections {
            if let Err(error) = connection.client.close().await {
                tracing::warn!(
                    event = "mcp.server.close.failed",
                    server_id = connection.id,
                    error = error.to_string(),
                );
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }

        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl Connection {
    async fn start(id: &str, config: &Server) -> anyhow::Result<Self> {
        tracing::info!(event = "mcp.server.starting", server_id = id);

        let client = Client::connect(config.stdio())
            .await
            .with_context(|| format!("failed to connect MCP server {id:?}"))?;
        let tools = match client.tools().await {
            Ok(tools) => tools,
            Err(error) => {
                close_client(id, &client).await;
                return Err(error)
                    .with_context(|| format!("failed to discover tools from MCP server {id:?}"));
            }
        };

        if let Err(error) = validate_tools(id, &tools) {
            close_client(id, &client).await;
            return Err(error);
        }

        tracing::info!(
            event = "mcp.server.started",
            server_id = id,
            tool_count = tools.len(),
        );

        Ok(Self {
            id: id.to_owned(),
            client,
            tools,
        })
    }
}

struct Tool {
    name: String,
    remote_name: String,
    description: String,
    input_schema: Value,
    client: Client,
}

impl Tool {
    fn new(connection: &Connection, tool: &RemoteTool) -> anyhow::Result<Self> {
        let name = exposed_name(&connection.id, &tool.name)?;
        let description = match &tool.description {
            Some(description) => format!("MCP server {}: {description}", connection.id),
            None => format!("Tool {} from MCP server {}", tool.name, connection.id),
        };

        Ok(Self {
            name,
            remote_name: tool.name.clone(),
            description,
            input_schema: tool.input_schema.clone(),
            client: connection.client.clone(),
        })
    }
}

#[async_trait::async_trait]
impl DynTool for Tool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> Value {
        self.input_schema.clone()
    }

    fn definition(&self) -> anyhow::Result<ToolDefinition> {
        Ok(ToolDefinition {
            name: self.name.clone(),
            description: self.description.clone(),
            arguments: self.input_schema.clone(),
        })
    }

    async fn call(&self, arguments: Value) -> anyhow::Result<ToolOutput<Value>> {
        let output = self.client.call(&self.remote_name, arguments).await?;
        Ok(ToolOutput::new(output.into_value()))
    }

    fn compact(&self, _summary: &Summary) -> anyhow::Result<Option<String>> {
        Ok(None)
    }

    async fn cancel(&self, _arguments: Value) -> anyhow::Result<()> {
        Ok(())
    }
}

fn validate_tools(server: &str, tools: &[RemoteTool]) -> anyhow::Result<()> {
    let mut names = HashSet::new();

    for tool in tools {
        anyhow::ensure!(
            names.insert(&tool.name),
            "MCP server {server:?} returned duplicate tool {:?}",
            tool.name
        );
        exposed_name(server, &tool.name)?;
    }

    Ok(())
}

fn exposed_name(server: &str, tool: &str) -> anyhow::Result<String> {
    anyhow::ensure!(!tool.is_empty(), "MCP tool name must not be empty");

    let name = format!("{server}__{tool}");
    let valid = name
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'));

    anyhow::ensure!(
        valid,
        "MCP tool name {name:?} may contain only ASCII letters, digits, underscores, and hyphens"
    );

    Ok(name)
}

async fn close_connections(connections: &[Connection]) {
    for connection in connections {
        close_client(&connection.id, &connection.client).await;
    }
}

async fn close_client(id: &str, client: &Client) {
    if let Err(error) = client.close().await {
        tracing::warn!(
            event = "mcp.server.close.failed",
            server_id = id,
            error = error.to_string(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn namespaces_tools_by_server() {
        assert_eq!(exposed_name("search", "query").unwrap(), "search__query");
    }

    #[test]
    fn rejects_names_the_provider_cannot_send() {
        let error = exposed_name("web.search", "query").unwrap_err();

        assert!(error.to_string().contains("ASCII letters"));
    }

    #[test]
    fn rejects_empty_remote_tool_names() {
        let error = exposed_name("search", "").unwrap_err();

        assert_eq!(error.to_string(), "MCP tool name must not be empty");
    }

    #[tokio::test]
    async fn disabled_servers_are_not_started() {
        let config = serde_json::from_value::<Config>(json!({
            "servers": {
                "disabled": {
                    "command": "/definitely/missing/mcp-server",
                    "enabled": false
                }
            }
        }))
        .unwrap();

        let runtime = Runtime::start(&config).await.unwrap();

        assert_eq!(runtime.server_count(), 0);
        runtime.close().await.unwrap();
    }

    #[tokio::test]
    async fn validates_configuration_before_starting_servers() {
        let config = serde_json::from_value::<Config>(json!({
            "servers": {
                "invalid.id": {
                    "command": "/definitely/missing/mcp-server",
                    "enabled": false
                }
            }
        }))
        .unwrap();

        let error = match Runtime::start(&config).await {
            Ok(_) => panic!("invalid server ids should fail startup"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("invalid MCP configuration"));
        assert!(format!("{error:#}").contains("MCP server id \"invalid.id\""));
    }

    #[tokio::test]
    async fn configured_startup_failures_name_the_server() {
        let config = serde_json::from_value::<Config>(json!({
            "servers": {
                "missing": {
                    "command": "/definitely/missing/mcp-server"
                }
            }
        }))
        .unwrap();

        let error = match Runtime::start(&config).await {
            Ok(_) => panic!("missing MCP executable should fail startup"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("MCP server \"missing\""));
    }
}
