//! Real MCP client checks across authenticated HTTP and the process-based stdio bridge.

#[allow(dead_code)]
mod support;

use agent_tasks_linear::{config::Config, server};
use rmcp::{
    ServiceExt,
    model::CallToolRequestParams,
    transport::{
        StreamableHttpClientTransport, TokioChildProcess,
        streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde_json::json;
use std::time::Duration;
use support::Fixture;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Compare success and error text over authenticated HTTP and stdio, including transport guards.
#[tokio::test]
async fn authenticated_http_and_stdio_share_the_gateway() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let token = "test-gateway-token-01234567890123456789";
    let config = Config {
        listen: address,
        token: token.into(),
    };
    let fixture = Fixture::new().await;
    let project_id = fixture.project().await;
    let gateway = fixture.gateway.clone();
    let cancellation = CancellationToken::new();
    let app = server::router(gateway, &config, cancellation.clone());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let url = format!("http://{address}/mcp");
    let http = reqwest::Client::new();
    assert_eq!(
        http.post(&url)
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let rejected=http.post(&url).bearer_auth(token).header("origin","https://untrusted.example").header("accept","application/json, text/event-stream").json(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-16","capabilities":{},"clientInfo":{"name":"test","version":"1"}}})).send().await.unwrap();
    assert!(!rejected.status().is_success());
    let transport = StreamableHttpClientTransport::with_client(
        http,
        StreamableHttpClientTransportConfig::with_uri(url).auth_header(token),
    );
    let client = tokio::time::timeout(Duration::from_secs(10), ().serve(transport))
        .await
        .unwrap()
        .unwrap();
    let list = client.peer().list_tools(None).await.unwrap();
    assert_eq!(list.tools.len(), 25);
    assert!(list.tools.iter().any(|t| t.name == "record_review"));
    assert!(list.tools.iter().any(|t| t.name == "get_overview"));
    let request = CallToolRequestParams::new("get_context").with_arguments(
        json!({"type":"project","id":project_id.clone()})
            .as_object()
            .unwrap()
            .clone(),
    );
    let outcome = client.peer().call_tool(request.clone()).await.unwrap();
    let wire = serde_json::to_value(outcome).unwrap();
    assert_eq!(wire["content"].as_array().unwrap().len(), 1);
    assert_eq!(wire["content"][0]["type"], "text");
    assert!(
        wire["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Workflow integration fixture")
    );
    assert_eq!(wire["isError"], false);
    assert!(wire.get("structuredContent").is_none());
    let structured = fixture
        .gateway
        .call("get_context", json!({"type":"project","id":project_id}))
        .await;
    let old_bytes = serde_json::to_vec(&structured).unwrap().len();
    let text_bytes = wire["content"][0]["text"].as_str().unwrap().len();
    eprintln!("fixture project context bytes: structured={old_bytes}, text={text_bytes}");
    assert!(text_bytes < old_bytes);
    let create_args = json!({"request_id":Uuid::new_v4().to_string(),"actor":"codex:transport","team_id":fixture.team,"title":"Second project","description":"A long creation description that should appear in a later read, not in this acknowledgement."});
    let structured_create = fixture
        .gateway
        .call("create_project", create_args.clone())
        .await;
    assert_eq!(structured_create.status, "ok");
    let created = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("create_project")
                .with_arguments(create_args.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    let created_wire = serde_json::to_value(created).unwrap();
    let old_bytes = serde_json::to_vec(&structured_create).unwrap().len();
    let text_bytes = created_wire["content"][0]["text"].as_str().unwrap().len();
    eprintln!("fixture project creation bytes: structured={old_bytes}, text={text_bytes}");
    assert!(text_bytes < old_bytes);
    assert_eq!(created_wire["isError"], false);
    assert!(created_wire.get("structuredContent").is_none());
    let invalid = CallToolRequestParams::new("get_context");
    let error =
        serde_json::to_value(client.peer().call_tool(invalid.clone()).await.unwrap()).unwrap();
    assert_eq!(error["content"].as_array().unwrap().len(), 1);
    assert_eq!(error["isError"], true);
    assert!(
        error["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("INVALID_INPUT")
    );
    assert!(error.get("structuredContent").is_none());
    let path = std::env::temp_dir().join(format!("atl-transport-{}.toml", Uuid::new_v4()));
    config.write_new(&path).unwrap();
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_agent-tasks-linear"));
    command.arg("--config").arg(&path).arg("stdio");
    let child = TokioChildProcess::new(command).unwrap();
    let bridge = tokio::time::timeout(Duration::from_secs(10), ().serve(child))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        bridge.peer().list_tools(None).await.unwrap().tools.len(),
        25
    );
    let stdio = serde_json::to_value(bridge.peer().call_tool(request).await.unwrap()).unwrap();
    assert_eq!(stdio["content"], wire["content"]);
    assert_eq!(stdio["isError"], false);
    assert!(stdio.get("structuredContent").is_none());
    let stdio_error =
        serde_json::to_value(bridge.peer().call_tool(invalid).await.unwrap()).unwrap();
    assert_eq!(stdio_error["content"], error["content"]);
    assert_eq!(stdio_error["isError"], true);
    assert!(stdio_error.get("structuredContent").is_none());
    bridge.cancel().await.unwrap();
    client.cancel().await.unwrap();
    std::fs::remove_file(path).unwrap();
    cancellation.cancel();
    task.abort();
}
