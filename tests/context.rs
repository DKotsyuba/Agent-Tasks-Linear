//! Context and overview contracts against the native HTTP fixture.
#[allow(dead_code)]
mod support;

use serde_json::json;
use support::Fixture;

/// A native Issue link opens a complete role view while legacy UUID calls retain their shape.
#[tokio::test]
async fn issue_link_and_role_views_preserve_legacy_context() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let epic = f.work("epic", &project, None).await;
    let module = f.work("module", &project, Some(&epic)).await;
    let task = f.work("task", &project, Some(&module)).await;
    let link = {
        let mut db = f.db.lock().await;
        let issue = db.issues.get_mut(&module).unwrap();
        let link = format!(
            "https://linear.app/example/issue/{}/readable-module",
            issue["identifier"].as_str().unwrap()
        );
        issue["url"] = json!(link);
        link
    };
    let question = f
        .ok(
            "add_comment",
            json!({"target_type":"issue","target_id":module,
        "kind":"question","role":"lead","recipient":"orchestrator","body":"Which release?"}),
        )
        .await;
    let legacy = f
        .ok("get_context", json!({"type":"issue","id":module}))
        .await;
    assert!(legacy["agent_context"].is_null());
    assert_eq!(legacy["module_report"]["tasks_total"], 1);
    let lead = f.ok("get_context", json!({"url":link,"view":"lead"})).await;
    let agent = &lead["agent_context"];
    assert_eq!(agent["view"], "lead");
    assert_eq!(agent["tasks"][0]["id"], task);
    assert_eq!(agent["epic"]["business_requirements"], "Business outcome");
    assert_eq!(agent["checkout"]["lead"], "codex:lead");
    assert_eq!(
        agent["open_questions"][0]["url"],
        question["comment"]["url"]
    );
    assert!(agent["documents"].as_array().unwrap().len() >= 2);
    let reviewer = f
        .ok(
            "get_context",
            json!({"id":link,"type":"issue","view":"reviewer"}),
        )
        .await;
    assert_eq!(reviewer["agent_context"]["view"], "reviewer");
    assert!(reviewer["agent_context"]["review_evidence"].is_object());
    let wrong = f
        .call(
            "get_context",
            json!({"url":"https://example.com/example/issue/TEST-1/x"}),
        )
        .await;
    assert_eq!(wrong.data["code"], "INVALID_LINK");
}

/// Missing recorded children suppress exact Module counts instead of reporting false zeros.
#[tokio::test]
async fn missing_recorded_child_is_explicitly_incomplete() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let module = f.work("module", &project, None).await;
    let task = f.work("task", &project, Some(&module)).await;
    f.db.lock().await.issues.remove(&task);
    let context = f
        .ok(
            "get_context",
            json!({"type":"issue","id":module,"view":"lead"}),
        )
        .await;
    assert!(context["module_report"].is_null());
    assert!(
        context["agent_context"]["discrepancies"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry.as_str().unwrap().contains("Recorded child"))
    );
}
