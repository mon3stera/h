use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use anyhow::Result;
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, ListToolsResult, MetaObject, PaginatedRequestParams,
        ServerCapabilities, ServerInfo, Tool as McpTool,
    },
    service::{RequestContext, RoleServer},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::task::JoinHandle;

use crate::{Client, Config};

#[derive(Debug, Deserialize, JsonSchema)]
struct EchoRequest {
    text: String,
}

#[derive(Debug, Clone)]
struct TestServer {
    #[expect(dead_code, reason = "tool_handler accesses the generated router")]
    tool_router: ToolRouter<Self>,
}

impl TestServer {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl TestServer {
    #[tool(description = "Return the supplied text and structured metadata")]
    fn echo(&self, Parameters(EchoRequest { text }): Parameters<EchoRequest>) -> CallToolResult {
        let mut result = CallToolResult::success(vec![ContentBlock::text(text.clone())]);
        result.structured_content = Some(json!({ "echo": text }));
        result.meta = Some(MetaObject(Map::from_iter([(
            "source".to_owned(),
            Value::String("test-server".to_owned()),
        )])));
        result
    }

    #[tool(description = "Return a tool-level failure")]
    fn fail(&self) -> CallToolResult {
        CallToolResult::error(vec![ContentBlock::text("expected failure")])
    }
}

#[tool_handler]
impl ServerHandler for TestServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }
}

#[derive(Debug, Clone)]
struct PaginatedServer {
    calls: Arc<AtomicUsize>,
}

impl ServerHandler for PaginatedServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }

    fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, rmcp::ErrorData>> + Send + '_ {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let cursor = request.and_then(|request| request.cursor);

        async move {
            let mut result = if cursor.as_deref() == Some("next") {
                ListToolsResult::with_all_items(vec![tool("alpha")])
            } else {
                ListToolsResult::with_all_items(vec![tool("zebra")])
            };
            if cursor.is_none() {
                result.next_cursor = Some("next".to_owned());
            }

            Ok(result)
        }
    }
}

fn tool(name: &'static str) -> McpTool {
    McpTool::new(
        name,
        format!("The {name} tool"),
        Arc::new(Map::from_iter([(
            "type".to_owned(),
            Value::String("object".to_owned()),
        )])),
    )
}

async fn start<S>(server: S) -> Result<(Client, JoinHandle<Result<()>>)>
where
    S: ServerHandler,
{
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server = tokio::spawn(async move {
        server.serve(server_transport).await?.waiting().await?;
        Ok(())
    });
    let client = Client::from_service(().serve(client_transport).await?);

    Ok((client, server))
}

async fn close(client: Client, server: JoinHandle<Result<()>>) -> Result<()> {
    client.close().await?;
    server.await??;

    Ok(())
}

#[tokio::test]
async fn initializes_and_discovers_all_pages_in_name_order() -> Result<()> {
    let calls = Arc::new(AtomicUsize::new(0));
    let server = PaginatedServer {
        calls: calls.clone(),
    };
    let (client, server) = start(server).await?;

    let tools = client.tools().await?;

    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["alpha", "zebra"]
    );
    close(client, server).await
}

#[tokio::test]
async fn converts_input_schema_to_owned_json() -> Result<()> {
    let (client, server) = start(TestServer::new()).await?;

    let tools = client.tools().await?;
    let echo = tools.iter().find(|tool| tool.name == "echo").unwrap();

    assert_eq!(echo.input_schema["type"], "object");
    assert_eq!(echo.input_schema["properties"]["text"]["type"], "string");
    close(client, server).await
}

#[tokio::test]
async fn forwards_arguments_and_preserves_the_complete_result() -> Result<()> {
    let (client, server) = start(TestServer::new()).await?;

    let output = client.call("echo", json!({ "text": "hello" })).await?;

    assert!(!output.is_error());
    assert_eq!(output.value()["content"][0]["text"], "hello");
    assert_eq!(output.value()["structuredContent"]["echo"], "hello");
    assert_eq!(output.value()["_meta"]["source"], "test-server");
    close(client, server).await
}

#[tokio::test]
async fn preserves_tool_error_status() -> Result<()> {
    let (client, server) = start(TestServer::new()).await?;

    let output = client.call("fail", Value::Null).await?;

    assert!(output.is_error());
    assert_eq!(output.value()["isError"], true);
    close(client, server).await
}

#[tokio::test]
async fn rejects_non_object_arguments() -> Result<()> {
    let (client, server) = start(TestServer::new()).await?;

    let error = client.call("echo", json!(["hello"])).await.unwrap_err();

    assert_eq!(
        error.to_string(),
        "MCP tool arguments must be a JSON object or null, got array"
    );
    close(client, server).await
}

#[tokio::test]
async fn closes_explicitly_and_idempotently() -> Result<()> {
    let (client, server) = start(TestServer::new()).await?;
    let clone = client.clone();

    assert!(!client.is_closed());
    let (first, second) = tokio::join!(client.close(), clone.close());

    first?;
    second?;
    assert!(client.is_closed());
    client.close().await?;
    server.await??;

    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn connects_to_a_configured_stdio_process() -> Result<()> {
    let script = r#"
while IFS= read -r line; do
    id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([^,}]*\).*/\1/p')
    case "$line" in
        *'"method":"initialize"'*)
            printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1.0.0"}}}\n' "$id"
            ;;
        *'"method":"tools/list"'*)
            printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","description":"Echo text","inputSchema":{"type":"object"}}]}}\n' "$id"
            ;;
        *'"method":"tools/call"'*)
            printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"ok"}],"isError":false}}\n' "$id"
            ;;
    esac
done
"#;
    let client = Client::connect(crate::Stdio::new("sh").args(["-c", script])).await?;

    let tools = client.tools().await?;
    let output = client.call("echo", json!({ "text": "hello" })).await?;

    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "echo");
    assert_eq!(output.value()["content"][0]["text"], "ok");
    client.close().await
}

#[test]
fn parses_server_configuration_and_builds_stdio() {
    let config = serde_json::from_value::<Config>(json!({
        "servers": {
            "search": {
                "command": "node",
                "args": ["server.mjs", "--stdio"],
                "env": { "API_KEY": "secret" },
                "cwd": "/srv/search",
                "tools": ["query", "fetch"]
            },
            "disabled": {
                "command": "false",
                "enabled": false
            }
        }
    }))
    .unwrap();

    config.validate().unwrap();

    let search = &config.servers()["search"];
    let stdio = search.stdio();

    assert!(search.enabled());
    assert_eq!(stdio.program, "node");
    assert_eq!(stdio.args, ["server.mjs", "--stdio"]);
    assert_eq!(
        stdio.env[std::ffi::OsStr::new("API_KEY")],
        std::ffi::OsStr::new("secret")
    );
    assert_eq!(
        stdio.cwd.as_deref(),
        Some(std::path::Path::new("/srv/search"))
    );
    assert_eq!(search.tools().unwrap(), ["query", "fetch"]);
    assert!(!config.servers()["disabled"].enabled());
}

#[test]
fn rejects_duplicate_configured_tools() {
    let config = serde_json::from_value::<Config>(json!({
        "servers": {
            "search": {
                "command": "node",
                "tools": ["query", "query"]
            }
        }
    }))
    .unwrap();

    let error = config.validate().unwrap_err();

    assert_eq!(
        error.to_string(),
        "mcp.servers.search.tools contains duplicate tool \"query\""
    );
}

#[test]
fn rejects_empty_server_commands() {
    let config = serde_json::from_value::<Config>(json!({
        "servers": {
            "search": { "command": " " }
        }
    }))
    .unwrap();

    let error = config.validate().unwrap_err();

    assert_eq!(
        error.to_string(),
        "mcp.servers.search.command must not be empty"
    );
}

#[test]
fn rejects_server_ids_that_cannot_namespace_tools() {
    let config = serde_json::from_value::<Config>(json!({
        "servers": {
            "web.search": {
                "command": "node"
            }
        }
    }))
    .unwrap();

    let error = config.validate().unwrap_err();

    assert!(error.to_string().contains("MCP server id \"web.search\""));
    assert!(error.to_string().contains("ASCII letters"));
}
