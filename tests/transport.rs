//! Real MCP client checks across authenticated HTTP and the process-based stdio bridge.

use agent_tasks_linear::{
    config::{Binding, Config},
    gateway::Gateway,
    linear::Linear,
    model::{Principal, Role},
    records::{Signer, Store},
    server,
};
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
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Exercise tool discovery, missing-token errors, bearer/Origin guards and the real stdio executable.
#[tokio::test]
async fn authenticated_http_and_stdio_share_the_gateway() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let token = "test-gateway-token-01234567890123456789";
    let config = Config {
        listen: address,
        signing_key: "test-signing-key-01234567890123456789".into(),
        bindings: vec![Binding {
            name: "owner".into(),
            token: token.into(),
            principal: Principal {
                id: "owner".into(),
                role: Role::Owner,
                products: vec![],
                assignment_id: None,
                generation: None,
                epoch: 1,
            },
        }],
    };
    let gateway = Gateway::new(Store {
        linear: Linear::new(None, false).unwrap(),
        signer: Signer::new(&config.signing_key).unwrap(),
    })
    .unwrap();
    let cancellation = CancellationToken::new();
    let app = server::router(gateway, &config, cancellation.clone());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let url = format!("http://{address}/mcp/owner");
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
    let rejected=http.post(&url).bearer_auth(token).header("origin","https://untrusted.example").header("accept","application/json, text/event-stream").json(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"1"}}})).send().await.unwrap();
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
    assert!(!list.tools.iter().any(|t| t.name == "at_review_report"));
    let request = CallToolRequestParams::new("at_resume").with_arguments(
        json!({"product_id":Uuid::new_v4().to_string()})
            .as_object()
            .unwrap()
            .clone(),
    );
    let outcome = client.peer().call_tool(request).await.unwrap();
    let wire = serde_json::to_value(outcome).unwrap();
    assert_eq!(wire["structuredContent"]["status"], "unavailable");
    client.cancel().await.unwrap();
    let path = std::env::temp_dir().join(format!("atl-transport-{}.toml", Uuid::new_v4()));
    config.write_new(&path).unwrap();
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_agent-tasks-linear"));
    command
        .arg("--config")
        .arg(&path)
        .args(["stdio", "--binding", "owner"]);
    let child = TokioChildProcess::new(command).unwrap();
    let bridge = tokio::time::timeout(Duration::from_secs(10), ().serve(child))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        bridge.peer().list_tools(None).await.unwrap().tools.len(),
        25
    );
    bridge.cancel().await.unwrap();
    std::fs::remove_file(path).unwrap();
    cancellation.cancel();
    task.abort();
}
