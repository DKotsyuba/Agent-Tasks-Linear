//! Focused checks for the native file transport: signed upload reservation, the
//! signed PUT and the authenticated canonical-host download. No Gateway tool is
//! exercised here; only `Linear`'s additive transport methods.

use agent_tasks_linear::linear::{FILE_SIZE_CAP, Linear};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post, put},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::sync::Mutex;

/// What the fixture upload endpoint observed on its most recent PUT.
#[derive(Default)]
struct Recorded {
    headers: HeaderMap,
    body: Vec<u8>,
}

/// Shared fixture state: the recorded upload plus bytes served for download.
#[derive(Default)]
struct State_ {
    recorded: Recorded,
    asset_body: Vec<u8>,
}

/// Start a loopback fixture exposing GraphQL, a signed-upload PUT route and a
/// canonical-host asset GET route on the same origin, and return the ready `Linear`.
async fn fixture() -> (
    Linear,
    String,
    Arc<Mutex<State_>>,
    tokio::task::JoinHandle<()>,
) {
    let state = Arc::new(Mutex::new(State_::default()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route(
            "/",
            post({
                let base = base.clone();
                move |Json(request): Json<Value>| {
                    let base = base.clone();
                    async move {
                        assert_eq!(request["operationName"], "MFileUpload");
                        let v = &request["variables"];
                        Json(json!({"data":{"fileUpload":{"success":true,"uploadFile":{
                            "assetUrl": format!("{base}/asset/{}", v["filename"].as_str().unwrap()),
                            "uploadUrl": format!("{base}/upload/{}", v["filename"].as_str().unwrap()),
                            "headers": [{"key":"x-artifact-fixture","value":"present"}],
                            "contentType": v["contentType"],
                            "filename": v["filename"],
                            "size": v["size"],
                        }}}}))
                    }
                }
            }),
        )
        .route(
            "/upload/{name}",
            put(
                |State(state): State<Arc<Mutex<State_>>>,
                 Path(name): Path<String>,
                 headers: HeaderMap,
                 body: Bytes| async move {
                    if name == "reject.bin" {
                        return StatusCode::FORBIDDEN;
                    }
                    let mut state = state.lock().await;
                    state.recorded = Recorded {
                        headers,
                        body: body.to_vec(),
                    };
                    StatusCode::OK
                },
            ),
        )
        .route(
            "/asset/{name}",
            get(
                |State(state): State<Arc<Mutex<State_>>>,
                 Path(_name): Path<String>,
                 headers: HeaderMap| async move {
                    assert_eq!(
                        headers.get("authorization").map(|v| v.to_str().unwrap()),
                        Some("fixture"),
                    );
                    state.lock().await.asset_body.clone()
                },
            ),
        )
        .with_state(state.clone());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let linear = Linear::mock(&format!("{base}/")).unwrap();
    (linear, base, state, task)
}

#[tokio::test]
async fn reserve_upload_returns_the_native_descriptor() {
    let (linear, base, _state, task) = fixture().await;
    let upload_file = linear
        .reserve_upload("application/pdf", "report.pdf", 12)
        .await
        .unwrap();
    assert_eq!(upload_file["assetUrl"], format!("{base}/asset/report.pdf"));
    assert_eq!(
        upload_file["uploadUrl"],
        format!("{base}/upload/report.pdf")
    );
    assert_eq!(upload_file["contentType"], "application/pdf");
    assert_eq!(upload_file["size"], 12);
    task.abort();
}

#[tokio::test]
async fn reserve_upload_rejects_oversized_files_before_any_network_call() {
    // An unreachable endpoint proves no request was attempted.
    let linear = Linear::mock("http://127.0.0.1:1/").unwrap();
    let error = linear
        .reserve_upload("application/pdf", "huge.bin", FILE_SIZE_CAP + 1)
        .await
        .unwrap_err();
    assert_eq!(error.code, "INVALID_INPUT");
}

#[tokio::test]
async fn put_upload_sends_declared_headers_and_body_without_authorization() {
    let (linear, _base, state, task) = fixture().await;
    let upload_file = linear
        .reserve_upload("text/plain", "notes.txt", 5)
        .await
        .unwrap();
    linear
        .put_upload(&upload_file, b"hello".to_vec())
        .await
        .unwrap();
    let recorded = state.lock().await;
    assert_eq!(recorded.recorded.body, b"hello");
    assert_eq!(
        recorded.recorded.headers.get("x-artifact-fixture").unwrap(),
        "present"
    );
    assert!(recorded.recorded.headers.get("authorization").is_none());
    task.abort();
}

/// Reservation metadata supplies the media type while omitted headers receive upload defaults.
#[tokio::test]
async fn put_upload_sets_default_content_type_and_cache_control_when_headers_omit_them() {
    let (linear, base, state, task) = fixture().await;
    let upload_file = json!({
        "uploadUrl": format!("{base}/upload/defaults.bin"),
        "contentType": "application/pdf",
        "headers": [],
    });
    linear
        .put_upload(&upload_file, b"hello".to_vec())
        .await
        .unwrap();
    let recorded = state.lock().await;
    assert_eq!(
        recorded.recorded.headers.get("content-type").unwrap(),
        "application/pdf"
    );
    assert_eq!(
        recorded.recorded.headers.get("cache-control").unwrap(),
        "public, max-age=31536000"
    );
    task.abort();
}

/// An explicit reservation header replaces the corresponding default instead of being appended.
#[tokio::test]
async fn put_upload_lets_a_returned_header_override_the_default() {
    let (linear, base, state, task) = fixture().await;
    let upload_file = json!({
        "uploadUrl": format!("{base}/upload/override.bin"),
        "contentType": "application/pdf",
        "headers": [{"key":"Content-Type","value":"application/x-custom"}],
    });
    linear
        .put_upload(&upload_file, b"hello".to_vec())
        .await
        .unwrap();
    let recorded = state.lock().await;
    assert_eq!(
        recorded.recorded.headers.get("content-type").unwrap(),
        "application/x-custom"
    );
    task.abort();
}

#[tokio::test]
async fn put_upload_reports_a_clean_rejection_as_not_uncertain() {
    let (linear, base, _state, task) = fixture().await;
    let upload_file = json!({
        "uploadUrl": format!("{base}/upload/reject.bin"),
        "headers": [],
    });
    let error = linear
        .put_upload(&upload_file, b"x".to_vec())
        .await
        .unwrap_err();
    assert_eq!(error.code, "LINEAR_PARTIAL_ERROR");
    assert!(!error.uncertain);
    task.abort();
}

#[tokio::test]
async fn put_upload_marks_a_lost_response_as_uncertain() {
    let linear = Linear::mock("http://127.0.0.1:1/").unwrap();
    let upload_file = json!({"uploadUrl":"http://127.0.0.1:1/upload/x","headers":[]});
    let error = linear
        .put_upload(&upload_file, b"x".to_vec())
        .await
        .unwrap_err();
    assert_eq!(error.code, "LINEAR_UNAVAILABLE");
    assert!(error.uncertain);
}

#[tokio::test]
async fn get_asset_streams_bytes_and_matches_their_digest() {
    let (linear, base, state, task) = fixture().await;
    let content = b"binary-ish report content".to_vec();
    state.lock().await.asset_body = content.clone();
    let (bytes, digest) = linear
        .get_asset(&format!("{base}/asset/report.pdf"))
        .await
        .unwrap();
    assert_eq!(bytes, content);
    let mut expected = Sha256::new();
    expected.update(&content);
    assert_eq!(digest, format!("{:x}", expected.finalize()));
    task.abort();
}

#[tokio::test]
async fn get_asset_refuses_a_non_canonical_host() {
    let (linear, _base, _state, task) = fixture().await;
    let error = linear
        .get_asset("http://evil.example/asset/report.pdf")
        .await
        .unwrap_err();
    assert_eq!(error.code, "INVALID_INPUT");
    task.abort();
}

#[tokio::test]
async fn get_asset_refuses_a_download_over_the_product_cap() {
    let (linear, base, state, task) = fixture().await;
    state.lock().await.asset_body = vec![0u8; (FILE_SIZE_CAP + 1) as usize];
    let error = linear
        .get_asset(&format!("{base}/asset/huge.bin"))
        .await
        .unwrap_err();
    assert_eq!(error.code, "INCOMPLETE_DATA");
    task.abort();
}
