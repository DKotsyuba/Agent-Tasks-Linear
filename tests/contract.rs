//! Schema-first contract checks: the committed `schemas/tools.json` authority
//! must equal the embedded catalogue, the gateway dispatch vocabulary and the
//! discovery served by the real binary over stdio. Run via
//! `cargo xtask contract check`; `MCP_TEST_BINARY` can point acceptance at an
//! exact packaged/CI payload instead of the locally built binary.

#[allow(dead_code)]
mod support;

use agent_tasks_linear::{catalog::Catalog, config::Config, dispatch_vocabulary, routes, server};
use rmcp::{ServiceExt, transport::TokioChildProcess};
use serde_json::Value;
use std::time::Duration;
use support::Fixture;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// The committed authoritative catalogue.
fn schema_tools() -> Vec<Value> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("schemas/tools.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// The exact product binary under test: the packaged/CI payload when
/// MCP_TEST_BINARY is set, otherwise the locally built executable.
fn product_binary() -> String {
    std::env::var("MCP_TEST_BINARY")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_agent-tasks-linear").to_owned())
}

/// The embedded catalogue equals the committed schema file byte-for-byte in
/// meaning, including discovery order.
#[test]
fn embedded_catalogue_equals_committed_schema() {
    let embedded = Catalog::new().unwrap().tools;
    let committed = schema_tools();
    assert_eq!(embedded.len(), 25);
    assert_eq!(embedded, committed);
}

/// Every catalogue tool is routable and the dispatch vocabulary equals the
/// catalogue exactly, in order, with no unreachable extras.
#[test]
fn dispatch_vocabulary_equals_catalogue() {
    let names: Vec<String> = schema_tools()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    assert!(names.iter().all(|n| routes(n)));
    assert_eq!(dispatch_vocabulary(), names);
}

/// The real binary serving stdio discovery returns exactly the committed
/// schema: same names, order, descriptions, input schemas and annotations.
#[tokio::test]
async fn real_binary_stdio_discovery_equals_committed_schema() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let config = Config {
        listen: address,
        token: "contract-gateway-token-0123456789".into(),
    };
    let fixture = Fixture::new().await;
    let cancellation = CancellationToken::new();
    let app = server::router(fixture.gateway.clone(), &config, cancellation.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let path = std::env::temp_dir().join(format!("atl-contract-{}.toml", Uuid::new_v4()));
    config.write_new(&path).unwrap();
    let mut command = tokio::process::Command::new(product_binary());
    command.arg("--config").arg(&path).arg("stdio");
    let bridge = tokio::time::timeout(
        Duration::from_secs(10),
        ().serve(TokioChildProcess::new(command).unwrap()),
    )
    .await
    .unwrap()
    .unwrap();
    let listed = bridge.peer().list_tools(None).await.unwrap();
    let committed = schema_tools();
    assert_eq!(listed.tools.len(), committed.len());
    for (tool, schema) in listed.tools.iter().zip(committed.iter()) {
        let wire = serde_json::to_value(tool).unwrap();
        assert_eq!(
            wire["name"], schema["name"],
            "discovery order or name changed"
        );
        assert_eq!(
            wire["description"], schema["description"],
            "description drifted for {}",
            schema["name"]
        );
        assert_eq!(
            wire["inputSchema"], schema["inputSchema"],
            "input schema drifted for {}",
            schema["name"]
        );
        assert_eq!(
            wire["annotations"], schema["annotations"],
            "annotations drifted for {}",
            schema["name"]
        );
    }
    bridge.cancel().await.unwrap();
    std::fs::remove_file(path).unwrap();
    cancellation.cancel();
    task.abort();
}
