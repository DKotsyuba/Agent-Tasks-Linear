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

/// Revisions exercised over both public transports, oldest first.
const REVISIONS: &[&str] = &[
    "2024-11-05",
    "2025-03-26",
    "2025-06-18",
    "2025-11-25",
    "2026-07-28",
];

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
/// An empty stream must exit promptly; EOF before initialization may exit nonzero.
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
        let status = child.wait().await?;
        assert!(
            requests.is_empty() || status.success(),
            "stdio bridge failed on EOF: {status}"
        );
        Ok(replies)
    })
    .await?
}

/// Send raw HTTP requests with revision/method/name headers and legacy session
/// continuity. Responses accept JSON or SSE, are bounded at 1 MiB, and correlated
/// by id. Notifications must return 202. A 20-second deadline covers the complete
/// exchange; only the fixture bearer token is sent to its loopback listener.
async fn raw_http(config: &Config, revision: &str, requests: &[Value]) -> TestResult<Vec<Value>> {
    tokio::time::timeout(Duration::from_secs(20), async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()?;
        let url = format!("http://{}/mcp", config.listen);
        let mut session = None;
        let mut replies = Vec::new();
        for request in requests {
            let mut post = client
                .post(&url)
                .bearer_auth(&config.token)
                .header("accept", "application/json, text/event-stream")
                .header("MCP-Protocol-Version", revision)
                .header(
                    "Mcp-Method",
                    request["method"].as_str().ok_or("missing method")?,
                )
                .json(request);
            if let Some(session) = &session {
                post = post.header("Mcp-Session-Id", session);
            }
            if let Some(name) = request["params"]["name"].as_str() {
                post = post.header("Mcp-Name", name);
            }
            let mut response = post.send().await?;
            assert!(
                response.status().is_success()
                    || (request["params"]["name"] == "unknown_tool" && response.status() == 404),
                "HTTP {revision}: {}",
                response.status()
            );
            if let Some(value) = response.headers().get("Mcp-Session-Id") {
                session = Some(value.to_str()?.to_owned());
            }
            if request.get("id").is_none() {
                assert_eq!(response.status(), 202);
                continue;
            }
            let sse = response
                .headers()
                .get("content-type")
                .is_some_and(|v| v.to_str().is_ok_and(|v| v.starts_with("text/event-stream")));
            let mut bytes = Vec::new();
            let reply = loop {
                let chunk = response
                    .chunk()
                    .await?
                    .ok_or("HTTP EOF before JSON-RPC reply")?;
                assert!(
                    bytes.len() + chunk.len() <= 1_048_576,
                    "HTTP reply exceeds 1 MiB"
                );
                bytes.extend_from_slice(&chunk);
                if sse {
                    if let Ok(text) = std::str::from_utf8(&bytes)
                        && let Some(data) =
                            text.lines().find_map(|line| line.strip_prefix("data: "))
                        && let Ok(reply) = serde_json::from_str::<Value>(data)
                    {
                        break reply;
                    }
                } else if let Ok(reply) = serde_json::from_slice::<Value>(&bytes) {
                    break reply;
                }
            };
            assert_eq!(reply["id"], request["id"]);
            assert_eq!(reply["jsonrpc"], "2.0");
            replies.push(reply);
        }
        Ok(replies)
    })
    .await?
}

/// Extend discovery with valid read, invalid input and unknown name requests.
/// Project id is the fixture's existing native UUID; no external write occurs.
fn protocol_requests(revision: &str, project_id: &str) -> Vec<Value> {
    let mut requests = catalog_requests(revision);
    for (id, params) in [
        (
            3,
            json!({"name":"get_context","arguments":{"type":"project","id":project_id}}),
        ),
        (4, json!({"name":"get_context","arguments":{}})),
        (5, json!({"name":"unknown_tool","arguments":{}})),
    ] {
        requests.push(request(revision, id, "tools/call", params));
    }
    requests
}

/// Verify discovery/negotiation, exact catalogue, business results and protocol
/// errors. Modern discovery must advertise exactly the revisions tested here.
fn assert_protocol(replies: &[Value], revision: &str) -> TestResult {
    if revision == "2026-07-28" {
        assert_eq!(replies[0]["result"]["supportedVersions"], json!(REVISIONS));
        assert_eq!(replies[0]["result"]["ttlMs"], 0);
        assert_eq!(replies[0]["result"]["cacheScope"], "private");
    } else {
        assert_eq!(replies[0]["result"]["protocolVersion"], revision);
    }
    assert_catalog(&replies[1], revision)?;
    let success = &replies[2]["result"];
    assert_eq!(success["isError"], false);
    assert!(
        success["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("Workflow integration fixture"))
    );
    let invalid = &replies[3]["result"];
    assert_eq!(invalid["isError"], true);
    assert!(
        invalid["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("INVALID_INPUT"))
    );
    assert_eq!(replies[4]["error"]["code"], -32601);
    assert!(replies[4].get("result").is_none());
    Ok(())
}

/// Exercise modern no-initialize and explicit legacy catalogue paths through
/// HTTP and the real bridge, including calls and clean/empty-input EOF.
/// The bridge's upstream revision differs from several callers, detecting
/// metadata inherited from the upstream rather than the actual local caller.
/// Family and registration declarations must equal the exercised revisions.
#[tokio::test]
async fn raw_http_and_stdio_protocol_revisions() -> TestResult {
    let family: toml::Value = toml::from_str(include_str!("../family.toml"))?;
    let registration: Value =
        serde_json::from_str(include_str!("../registration/agent-tasks.json"))?;
    assert_eq!(
        serde_json::to_value(&family["compatibility"]["supported_protocol_revisions"])?,
        json!(REVISIONS)
    );
    assert_eq!(
        registration["supported_protocol_revisions"],
        json!(REVISIONS)
    );
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
    let project_id = fixture.project().await;
    for revision in REVISIONS {
        let requests = protocol_requests(revision, &project_id);
        let stdio = raw_stdio(&path, home.path(), &requests).await?;
        assert_protocol(&stdio, revision)?;
        let http = raw_http(&config, revision, &requests).await?;
        assert_protocol(&http, revision)?;
    }
    assert!(raw_stdio(&path, home.path(), &[]).await?.is_empty());
    let direct = raw_stdio(
        &path,
        home.path(),
        &[request("2026-07-28", 1, "tools/list", json!({}))],
    )
    .await?;
    assert_catalog(&direct[0], "2026-07-28")?;
    cancellation.cancel();
    task.abort();
    Ok(())
}
