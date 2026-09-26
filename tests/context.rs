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

/// A mixed Project yields exact progress and an unpublished draft; publication stays explicit.
#[tokio::test]
async fn overview_groups_work_and_only_explicit_update_writes() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let epic = f.work("epic", &project, None).await;
    let module = f.work("module", &project, Some(&epic)).await;
    f.mv(&epic, "In Progress").await;
    f.mv(&module, "In Progress").await;
    let done = f.work("task", &project, Some(&module)).await;
    f.mv(&done, "In Progress").await;
    f.result("task", &done).await;
    f.mv(&done, "Done").await;
    let excluded = f.work("task", &project, Some(&module)).await;
    f.ok(
        "edit_task",
        json!({"id":excluded,"fields":{"reason":"Superseded"}}),
    )
    .await;
    f.mv(&excluded, "Canceled").await;
    let standalone = f.work("module", &project, None).await;
    f.mv(&standalone, "In Progress").await;
    let atomic = f.work("atomic", &project, None).await;
    let question = f
        .ok(
            "add_comment",
            json!({"target_type":"issue","target_id":done,
        "kind":"question","role":"worker","recipient":"lead","body":"Ship this result?"}),
        )
        .await;
    let before = {
        let db = f.db.lock().await;
        (db.tick, db.comments.len(), db.project_updates.len())
    };
    let overview = f.ok("get_overview", json!({"project_id":project})).await;
    let after = f.db.lock().await;
    assert_eq!(
        (
            after.tick,
            after.comments.len(),
            after.project_updates.len()
        ),
        before
    );
    drop(after);
    assert_eq!(overview["active_epics"][0]["id"], epic);
    assert_eq!(overview["active_epics"][0]["modules"][0]["id"], module);
    assert_eq!(overview["active_epics"][0]["tasks_done"], 1);
    assert_eq!(overview["active_epics"][0]["tasks_total"], 1);
    assert_eq!(overview["standalone_modules"][0]["id"], standalone);
    assert_eq!(overview["atomics"][0]["id"], atomic);
    assert_eq!(overview["excluded"][0]["id"], excluded);
    assert_eq!(
        overview["open_questions"][0]["url"],
        question["comment"]["url"]
    );
    assert!(
        overview["project_update_draft"]
            .as_str()
            .unwrap()
            .contains("Tasks Done")
    );
    let update_args = json!({"request_id":support::id(),"project_id":project,"health":"onTrack",
        "reason":"Progress verified"});
    let update = f.ok("save_project_update", update_args.clone()).await;
    assert_eq!(update["activity"]["health"], "onTrack");
    assert!(
        update["activity"]["body"]
            .as_str()
            .unwrap()
            .contains("Project overview")
    );
    assert_eq!(f.db.lock().await.project_updates.len(), before.2 + 1);
    f.ok("add_comment", json!({"target_type":"project","target_id":project,
        "kind":"question","role":"worker","recipient":"lead","body":"New question after publication"})).await;
    let replay = f.ok("save_project_update", update_args).await;
    assert_eq!(replay["replayed"], true);
    assert_eq!(
        replay["project_update"]["id"],
        update["project_update"]["id"]
    );
    assert_eq!(f.db.lock().await.project_updates.len(), before.2 + 1);
    let edit_without_body = f
        .call(
            "save_project_update",
            json!({"id":update["project_update"]["id"],
        "project_id":project,"health":"onTrack","reason":"Progress verified",
        "expected_updated_at":update["project_update"]["updatedAt"]}),
        )
        .await;
    assert_eq!(edit_without_body.data["code"], "INVALID_INPUT");
}

/// A missing direct child blocks overview composition instead of lowering Task totals.
#[tokio::test]
async fn overview_rejects_incomplete_membership() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let module = f.work("module", &project, None).await;
    f.mv(&module, "In Progress").await;
    let task = f.work("task", &project, Some(&module)).await;
    f.db.lock().await.issues.remove(&task);
    let outcome = f.call("get_overview", json!({"project_id":project})).await;
    assert_eq!(outcome.data["code"], "INCOMPLETE_DATA");
}

/// Only a retained same-Project baseline may justify an empty or changed delta.
#[tokio::test]
async fn overview_delta_tracks_work_and_discussion_with_safe_fallbacks() {
    let mut f = Fixture::new().await;
    let project = f.project().await;
    let module = f.work("module", &project, None).await;
    f.mv(&module, "In Progress").await;
    let task = f.work("task", &project, Some(&module)).await;
    f.mv(&task, "In Progress").await;
    let first = f.ok("get_overview", json!({"project_id":project})).await;
    assert_eq!(first["baseline_expired"], false);
    assert!(first["changes"].is_null());
    let unchanged = f
        .ok(
            "get_overview",
            json!({"project_id":project,"cursor":first["cursor"]}),
        )
        .await;
    assert_eq!(unchanged["changes"], json!([]));
    assert_ne!(unchanged["cursor"], first["cursor"]);
    f.ok(
        "edit_module",
        json!({"id":module,"fields":{"lead":"codex:new-lead"}}),
    )
    .await;
    f.ok("edit_task", json!({"id":task,"fields":{"result":"Finished work","check_result":"Manual check passed","artifact_url":"https://example.test/result"}})).await;
    f.mv(&task, "Done").await;
    let question = f
        .ok(
            "add_comment",
            json!({"target_type":"issue","target_id":module,
        "kind":"question","role":"lead","recipient":"reviewer","body":"Ready for review?"}),
        )
        .await;
    let changed = f
        .ok(
            "get_overview",
            json!({"project_id":project,"cursor":unchanged["cursor"]}),
        )
        .await;
    let entries = changed["changes"].as_array().unwrap();
    assert!(
        entries
            .iter()
            .any(|entry| entry["key"] == format!("work:{module}")
                && entry["after"]["lead"] == "codex:new-lead")
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry["key"] == format!("work:{task}")
                && entry["after"]["status"] == "Done"
                && entry["after"]["result_preview"] == "Finished work")
    );
    assert!(
        entries.iter().any(|entry| entry["key"]
            == format!("activity:{}", question["comment"]["id"].as_str().unwrap()))
    );
    let reply = f
        .ok(
            "add_comment",
            json!({"target_type":"issue","target_id":module,
        "parent_id":question["comment"]["id"],"kind":"note","role":"reviewer","body":"Yes"}),
        )
        .await;
    f.ok(
        "resolve_comment",
        json!({"id":question["comment"]["id"],"resolved":true,
        "resolving_comment_id":reply["comment"]["id"]}),
    )
    .await;
    let discussion = f
        .ok(
            "get_overview",
            json!({"project_id":project,"cursor":changed["cursor"]}),
        )
        .await;
    assert!(
        discussion["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["key"]
                == format!("activity:{}", question["comment"]["id"].as_str().unwrap())
                && entry["after"]["resolved_at"].is_string())
    );
    assert!(
        discussion["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["key"]
                == format!("activity:{}", reply["comment"]["id"].as_str().unwrap()))
    );
    f.result("module", &module).await;
    f.mv(&module, "In Review").await;
    f.review(&module, "accepted").await;
    let reviewed = f
        .ok(
            "get_overview",
            json!({"project_id":project,"cursor":discussion["cursor"]}),
        )
        .await;
    assert!(
        reviewed["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["key"] == format!("work:{module}")
                && entry["after"]["review"]["accepted"] == true)
    );
    let other = f.project().await;
    let foreign = f
        .ok(
            "get_overview",
            json!({"project_id":other,"cursor":reviewed["cursor"]}),
        )
        .await;
    assert_eq!(foreign["baseline_expired"], true);
    assert!(foreign["changes"].is_null());
    assert_eq!(foreign["project_id"], other);
    f.restart();
    let cold = f
        .ok(
            "get_overview",
            json!({"project_id":project,"cursor":reviewed["cursor"]}),
        )
        .await;
    assert_eq!(cold["baseline_expired"], true);
    assert!(cold["changes"].is_null());
    assert!(cold["standalone_modules"].is_array());
}
