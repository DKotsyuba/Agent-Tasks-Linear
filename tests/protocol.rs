//! Bounded raw JSON-RPC checks against the real stdio bridge and fixture writer.
//! SDK compatibility evidence does not certify an external host or live Linear.
#[allow(dead_code)]
mod support;

use agent_tasks::{config::Config, server};
use serde_json::{Value, json};
use std::{path::Path, process::Stdio, time::Duration};
use support::Fixture;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

/// Propagate setup/transport failures as test failures without unchecked extraction.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// Build a request for a tested revision. Modern requests declare their version
/// and empty capabilities per request; legacy requests use the initialized session.
/// The supplied id, method and object params are copied without I/O.
fn request(revision: &str, id: u64, method: &str, mut params: Value) -> Value {
    if revision == "2026-07-28" {
        params["_meta"] = json!({
            "io.modelcontextprotocol/protocolVersion": revision,
            "io.modelcontextprotocol/clientCapabilities": {}
        });
    }
    json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params})
}

/// Build discovery followed by tools/list. Modern discovery needs no initialize;
/// legacy revisions explicitly complete initialize/initialized before tools/list.
fn catalog_requests(revision: &str) -> Vec<Value> {
    let mut requests = if revision == "2026-07-28" {
        vec![request(revision, 1, "server/discover", json!({}))]
    } else {
        vec![
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":revision,"capabilities":{},
                "clientInfo":{"name":"raw-test","version":"1"}
            }}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        ]
    };
    requests.push(request(revision, 2, "tools/list", json!({})));
    requests
}

/// Require the byte-authoritative catalogue's parsed value and revision-specific
/// metadata. Legacy must omit every modern field; modern must explicitly supply it.
fn assert_catalog(reply: &Value, revision: &str) -> TestResult {
    assert!(reply.get("error").is_none(), "{reply}");
    let result = &reply["result"];
    let expected: Value = serde_json::from_str(include_str!("../schemas/tools.json"))?;
    assert_eq!(result["tools"], expected);
    if revision == "2026-07-28" {
        assert_eq!(result["resultType"], "complete");
        assert_eq!(result["ttlMs"].as_u64(), Some(60_000));
        assert_eq!(result["cacheScope"], "private");
    } else {
        for field in ["resultType", "ttlMs", "cacheScope"] {
            assert!(
                result.get(field).is_none(),
                "legacy {revision} {field}: {result}"
            );
        }
    }
    Ok(())
}

/// Exchange requests through the selected real payload in an isolated HOME.
/// Each stdout reply is capped at 1 MiB and correlated by id. Notifications have
/// no reply. A 20-second deadline covers reads and clean EOF shutdown; dropping
/// the child kills it on any failure. Only the explicit fixture config is read.
async fn raw_stdio(config: &Path, home: &Path, requests: &[Value]) -> TestResult<Vec<Value>> {
    let mut child = tokio::process::Command::new(support::product_binary())
        .arg("--config")
        .arg(config)
        .arg("mcp")
        .env_clear()
        .env("HOME", home)
        .current_dir(home)
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut input = child.stdin.take().ok_or("missing child stdin")?;
        let mut output = BufReader::new(child.stdout.take().ok_or("missing child stdout")?);
        let mut replies = Vec::new();
        for request in requests {
            input.write_all(format!("{request}\n").as_bytes()).await?;
            input.flush().await?;
            if request.get("id").is_some() {
                let mut line = Vec::new();
                (&mut output)
                    .take(1_048_577)
                    .read_until(b'\n', &mut line)
                    .await?;
                assert!(
                    line.len() <= 1_048_576 && line.last() == Some(&b'\n'),
                    "bounded JSON-RPC reply before EOF"
                );
                let reply: Value = serde_json::from_slice(&line)?;
                assert_eq!(reply["id"], request["id"]);
                assert_eq!(reply["jsonrpc"], "2.0");
                replies.push(reply);
            }
        }
        drop(input);
        let mut trailing = Vec::new();
        (&mut output)
            .take(1_048_577)
            .read_to_end(&mut trailing)
            .await?;
        assert!(trailing.is_empty(), "unexpected stdout after replies");
        assert!(child.wait().await?.success(), "stdio bridge failed on EOF");
        Ok(replies)
    })
    .await?
}

/// Exercise modern no-initialize and explicit legacy catalogue paths through
/// the real bridge. Its upstream SDK connection uses a different revision,
/// so assertions detect incorrectly inheriting the upstream metadata.
#[tokio::test]
async fn raw_stdio_catalog_revisions() -> TestResult {
    let fixture = Fixture::new().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let config = Config {
        listen: listener.local_addr()?,
        token: "raw-gateway-token-01234567890123456789".into(),
    };
    let home = tempfile::tempdir()?;
    let path = home.path().join("config.toml");
    config.write_new(&path)?;
    let cancellation = CancellationToken::new();
    let app = server::router(fixture.gateway.clone(), &config, cancellation.clone());
    let task = tokio::spawn(async move { axum::serve(listener, app).await });
    for revision in ["2026-07-28", "2025-11-25"] {
        let replies = raw_stdio(&path, home.path(), &catalog_requests(revision)).await?;
        if revision == "2026-07-28" {
            assert!(
                replies[0]["result"]["supportedVersions"]
                    .as_array()
                    .is_some_and(|versions| versions.iter().any(|v| v == revision))
            );
            assert_eq!(replies[0]["result"]["ttlMs"], 0);
            assert_eq!(replies[0]["result"]["cacheScope"], "private");
        } else {
            assert_eq!(replies[0]["result"]["protocolVersion"], revision);
        }
        assert_catalog(&replies[1], revision)?;
    }
    cancellation.cancel();
    task.abort();
    Ok(())
}
