//! Real end-to-end artifact scenarios: upload/list/get through the Gateway, replay and
//! conflict detection, workflow-state isolation, cross-session retrieval, lost-response
//! recovery and no-overwrite destination publication. The fixture's `/upload` and `/asset`
//! routes stand in for Linear's signed URL and canonical asset host.

#[allow(dead_code)]
mod support;

use serde_json::json;
use std::path::PathBuf;
use support::Fixture;

/// Write `bytes` to a fresh temporary file and return its absolute path.
fn source_file(bytes: &[u8]) -> PathBuf {
    let path = std::env::temp_dir().join(format!("atl-artifact-src-{}.bin", uuid::Uuid::new_v4()));
    std::fs::write(&path, bytes).unwrap();
    path
}
/// A fresh, not-yet-existing destination path under a private per-test directory.
fn destination() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("atl-artifact-dst-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("report.out")
}

#[tokio::test]
async fn uploads_and_retrieves_text_and_binary_files_from_a_fresh_session() {
    let mut fixture = Fixture::new().await;
    let project = fixture.project().await;
    let work_id = fixture.work("module", &project, None).await;

    let text_path = source_file(b"Result: preserved end to end.\n");
    let uploaded_text = fixture
        .ok(
            "upload_file",
            json!({"work_id":work_id,"path":text_path.to_str().unwrap(),"title":"Report"}),
        )
        .await;
    assert_eq!(uploaded_text["replayed"], false);
    assert_eq!(
        uploaded_text["file_name"],
        text_path.file_name().unwrap().to_str().unwrap()
    );
    assert_eq!(uploaded_text["content_type"], "application/octet-stream");
    assert_eq!(uploaded_text["work_id"], work_id);

    let binary_bytes: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
    let binary_path = source_file(&binary_bytes);
    let uploaded_binary = fixture
        .ok(
            "upload_file",
            json!({"work_id":work_id,"path":binary_path.to_str().unwrap()}),
        )
        .await;
    assert_eq!(uploaded_binary["replayed"], false);

    let listed = fixture.ok("list_files", json!({"work_id":work_id})).await;
    let nodes = listed["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 2);
    assert!(
        nodes
            .iter()
            .all(|n| n["id"] == uploaded_text["id"] || n["id"] == uploaded_binary["id"])
    );

    // A fresh session (new process memory, same native backing) retrieves both files.
    fixture.restart();
    let text_destination = destination();
    let got_text = fixture
        .ok(
            "get_file",
            json!({"id":uploaded_text["id"],"destination":text_destination.to_str().unwrap()}),
        )
        .await;
    assert_eq!(got_text["replayed"], false);
    assert_eq!(
        std::fs::read(&text_destination).unwrap(),
        b"Result: preserved end to end.\n"
    );

    let binary_destination = destination();
    fixture
        .ok(
            "get_file",
            json!({"id":uploaded_binary["id"],"destination":binary_destination.to_str().unwrap()}),
        )
        .await;
    assert_eq!(std::fs::read(&binary_destination).unwrap(), binary_bytes);
}

#[tokio::test]
async fn upload_file_replays_an_identical_retry_and_conflicts_on_changed_intent() {
    let fixture = Fixture::new().await;
    let project = fixture.project().await;
    let work_id = fixture.work("module", &project, None).await;
    let path = source_file(b"first content");
    let request_id = support::id();

    let first = fixture
        .ok(
            "upload_file",
            json!({"request_id":request_id,"actor":"codex:fixture","work_id":work_id,"path":path.to_str().unwrap()}),
        )
        .await;
    assert_eq!(first["replayed"], false);

    let retry = fixture
        .ok(
            "upload_file",
            json!({"request_id":request_id,"actor":"codex:fixture","work_id":work_id,"path":path.to_str().unwrap()}),
        )
        .await;
    assert_eq!(retry["replayed"], true);
    assert_eq!(retry["id"], first["id"]);

    // Changed local content under the same request_id is a conflicting intent.
    std::fs::write(&path, b"different content").unwrap();
    let conflict = fixture
        .call(
            "upload_file",
            json!({"request_id":request_id,"actor":"codex:fixture","work_id":work_id,"path":path.to_str().unwrap()}),
        )
        .await;
    assert_eq!(conflict.status, "blocked");
    assert_eq!(conflict.data["code"], "REQUEST_CONFLICT");
}

#[tokio::test]
async fn artifact_upload_is_isolated_from_the_workflow_state_attachment() {
    let fixture = Fixture::new().await;
    let project = fixture.project().await;
    let work_id = fixture.work("module", &project, None).await;
    // create_work already persisted the canonical `metadata.workflow` state attachment
    // sharing the same native attachment table as artifacts.
    let path = source_file(b"artifact bytes");
    fixture
        .ok(
            "upload_file",
            json!({"work_id":work_id,"path":path.to_str().unwrap()}),
        )
        .await;

    let listed = fixture.ok("list_files", json!({"work_id":work_id})).await;
    assert_eq!(listed["nodes"].as_array().unwrap().len(), 1);

    // The state attachment is still readable and unaffected by the artifact write.
    let context = fixture
        .ok("get_context", json!({"type":"issue","id":work_id}))
        .await;
    assert_eq!(context["issue"]["id"], work_id);
}

#[tokio::test]
async fn get_file_never_overwrites_different_content_but_confirms_identical_replay() {
    let fixture = Fixture::new().await;
    let project = fixture.project().await;
    let work_id = fixture.work("module", &project, None).await;
    let path = source_file(b"canonical content");
    let uploaded = fixture
        .ok(
            "upload_file",
            json!({"work_id":work_id,"path":path.to_str().unwrap()}),
        )
        .await;

    let dest = destination();
    std::fs::write(&dest, b"someone else's file").unwrap();
    let conflict = fixture
        .call(
            "get_file",
            json!({"id":uploaded["id"],"destination":dest.to_str().unwrap()}),
        )
        .await;
    assert_eq!(conflict.status, "blocked");
    assert_eq!(conflict.data["code"], "FILE_EXISTS");
    assert_eq!(std::fs::read(&dest).unwrap(), b"someone else's file");

    std::fs::write(&dest, b"canonical content").unwrap();
    let replayed = fixture
        .ok(
            "get_file",
            json!({"id":uploaded["id"],"destination":dest.to_str().unwrap()}),
        )
        .await;
    assert_eq!(replayed["replayed"], true);
}

#[tokio::test]
async fn upload_file_recovers_after_a_lost_attachment_creation_response() {
    let mut fixture = Fixture::new().await;
    let project = fixture.project().await;
    let work_id = fixture.work("module", &project, None).await;
    let path = source_file(b"recovered content");
    let request_id = support::id();
    fixture.db.lock().await.lose = Some("MCreateArtifact".into());

    let lost = fixture
        .call(
            "upload_file",
            json!({"request_id":request_id,"actor":"codex:fixture","work_id":work_id,"path":path.to_str().unwrap()}),
        )
        .await;
    assert_eq!(lost.status, "outcome_unknown");

    // A cold retry with the same request finds the attachment the lost response actually created.
    fixture.restart();
    let retried = fixture
        .ok(
            "upload_file",
            json!({"request_id":request_id,"actor":"codex:fixture","work_id":work_id,"path":path.to_str().unwrap()}),
        )
        .await;
    assert_eq!(retried["replayed"], true);
}

#[tokio::test]
async fn upload_file_rejects_files_over_the_product_cap_before_any_write() {
    use agent_tasks::linear::FILE_SIZE_CAP;
    let fixture = Fixture::new().await;
    let project = fixture.project().await;
    let work_id = fixture.work("module", &project, None).await;
    let path = source_file(&vec![0u8; (FILE_SIZE_CAP + 1) as usize]);
    let rejected = fixture
        .call(
            "upload_file",
            json!({"work_id":work_id,"path":path.to_str().unwrap()}),
        )
        .await;
    assert_eq!(rejected.status, "blocked");
    assert_eq!(rejected.data["code"], "INVALID_INPUT");
    assert_eq!(
        fixture.ok("list_files", json!({"work_id":work_id})).await["nodes"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}
