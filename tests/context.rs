//! Context and overview contracts against the native HTTP fixture.
#[allow(dead_code)]
mod support;

use serde_json::json;
use support::{Fixture, id};

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

/// Role views link Project and current/ancestor Issue documents without loading their bodies.
#[tokio::test]
async fn role_context_includes_issue_ancestry_documents() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let epic = f.work("epic", &project, None).await;
    let module = f.work("module", &project, Some(&epic)).await;
    let task = f.work("task", &project, Some(&module)).await;
    let project_doc = f
        .ok(
            "save_document",
            json!({"project_id":project,"title":"Project guide","content":"Large body"}),
        )
        .await;
    let epic_doc = f
        .ok(
            "save_document",
            json!({"issue_id":epic,"title":"Epic plan","content":"Large body"}),
        )
        .await;
    let module_doc = f
        .ok(
            "save_document",
            json!({"issue_id":module,"title":"Module contract","content":"Large body"}),
        )
        .await;
    let task_doc = f
        .ok(
            "save_document",
            json!({"issue_id":task,"title":"Task details","content":"Large body"}),
        )
        .await;
    f.db.lock()
        .await
        .documents
        .get_mut(epic_doc["id"].as_str().unwrap())
        .unwrap()["archivedAt"] = json!("2026-09-25T00:00:00Z");
    let module_context = f
        .ok(
            "get_context",
            json!({"type":"issue","id":module,"view":"lead"}),
        )
        .await;
    let module_links = module_context["agent_context"]["documents"]
        .as_array()
        .unwrap();
    for doc in [&project_doc, &epic_doc, &module_doc] {
        assert!(module_links.iter().any(|link| link["id"] == doc["id"]));
    }
    assert!(!module_links.iter().any(|link| link["id"] == task_doc["id"]));
    assert!(
        module_links
            .iter()
            .all(|link| link.get("content").is_none())
    );
    let task_context = f
        .ok(
            "get_context",
            json!({"type":"issue","id":task,"view":"reviewer"}),
        )
        .await;
    let task_links = task_context["agent_context"]["documents"]
        .as_array()
        .unwrap();
    for doc in [&project_doc, &epic_doc, &module_doc, &task_doc] {
        assert!(task_links.iter().any(|link| link["id"] == doc["id"]));
    }
    let ids: std::collections::BTreeSet<_> = task_links
        .iter()
        .map(|link| link["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), task_links.len());
}

/// Native permalinks resolve Project, Issue, Document and ProjectUpdate references to the
/// same identity as UUID calls, while wrong types, decorated links and unknown or ambiguous
/// ProjectUpdate short tokens fail explicitly.
#[tokio::test]
async fn native_permalinks_resolve_typed_references() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let epic = f.work("epic", &project, None).await;
    let project_slug = "passport-a1b2c3d4e5";
    let (issue_link, project_url) = {
        let mut db = f.db.lock().await;
        let issue = db.issues.get_mut(&epic).unwrap();
        let link = format!(
            "https://linear.app/example/issue/{}/readable-epic",
            issue["identifier"].as_str().unwrap()
        );
        issue["url"] = json!(link);
        let url = format!("https://linear.app/example/project/{project_slug}");
        db.projects.get_mut(&project).unwrap()["url"] = json!(url);
        (link, url)
    };
    let document = f
        .ok(
            "save_document",
            json!({"project_id":project,"title":"Guide","content":"Body"}),
        )
        .await;
    let document_url = {
        let mut db = f.db.lock().await;
        let slug = format!("guide-{}", &document["id"].as_str().unwrap()[..8]);
        let url = format!("https://linear.app/example/document/{slug}");
        db.documents
            .get_mut(document["id"].as_str().unwrap())
            .unwrap()["url"] = json!(url);
        url
    };
    let update = f
        .ok(
            "save_project_update",
            json!({"project_id":project,"health":"onTrack","reason":"Steady progress"}),
        )
        .await;
    let update_url = update["url"].as_str().unwrap().to_owned();
    assert!(update_url.contains("/activity#project-update-"));

    let project_by_link = f.ok("get_context", json!({"url":project_url})).await;
    let project_by_uuid = f
        .ok("get_context", json!({"type":"project","id":project}))
        .await;
    assert_eq!(
        project_by_link["project"]["id"],
        project_by_uuid["project"]["id"]
    );
    assert_eq!(
        f.ok("get_overview", json!({"project_id":project_url}))
            .await["project_id"],
        json!(project)
    );
    let document_context = f.ok("get_context", json!({"url":document_url})).await;
    assert_eq!(document_context["id"], document["id"]);
    let update_context = f.ok("get_context", json!({"url":update_url})).await;
    assert_eq!(
        update_context["project_update"]["id"],
        update["project_update"]["id"]
    );
    assert_eq!(
        update_context["activity"]["id"],
        update_context["project_update"]["id"]
    );
    let issue_context = f.ok("get_context", json!({"url":issue_link})).await;
    assert_eq!(issue_context["issue"]["id"], json!(epic));
    assert_eq!(issue_context["agent_context"]["view"], "lead");
    // A comment permalink with uppercase fragment hex still reads the same comment.
    let comment = f
        .ok(
            "add_comment",
            json!({"target_type":"issue","target_id":epic,"kind":"note","body":"Case check"}),
        )
        .await["comment"]
        .clone();
    let (comment_base, comment_fragment) =
        comment["url"].as_str().unwrap().split_once('#').unwrap();
    let (prefix, hash) = comment_fragment.split_once('-').unwrap();
    let uppercased = format!("{comment_base}#{prefix}-{}", hash.to_ascii_uppercase());
    let by_fragment = f.ok("get_comment", json!({"id":uppercased})).await;
    assert_eq!(by_fragment["comment"]["id"], comment["id"]);
    let typed_issue_context = f
        .ok("get_context", json!({"id":issue_link,"type":"issue"}))
        .await;
    assert_eq!(typed_issue_context["issue"]["id"], json!(epic));
    let module = f.work("module", &project, Some(&epic)).await;
    let listed = f
        .ok(
            "list_items",
            json!({"type":"issue","project_id":project_url,"parent_id":issue_link,"kind":"module"}),
        )
        .await;
    assert_eq!(listed["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(listed["nodes"][0]["id"], json!(module));

    for (arguments, code) in [
        // The catalogue already rejects a Project field carrying a foreign permalink shape.
        (json!({"project_id":issue_link}), "INVALID_INPUT"),
        (json!({"project_id":document_url}), "INVALID_INPUT"),
        (
            json!({"url":format!("{document_url}?refresh=1")}),
            "INVALID_LINK",
        ),
        (
            json!({"url":document_url.replace("https://", "https://user:secret@")}),
            "INVALID_LINK",
        ),
        (
            json!({"url":format!("{project_url}#comment-abcdefgh")}),
            "INVALID_LINK",
        ),
        (json!({"url":issue_link,"type":"project"}), "INVALID_LINK"),
        (
            json!({"url":format!("{project_url}/activity#project-update-deadbeef")}),
            "RECORD_MISSING",
        ),
        // Ports, a missing workspace name and a ProjectUpdate fragment without its
        // /activity tail are not supported native shapes.
        (
            json!({"url":"https://linear.app:8443/example/issue/TEST-1/x"}),
            "INVALID_LINK",
        ),
        (
            json!({"url":"https://linear.app/issue/TEST-1/x"}),
            "INVALID_LINK",
        ),
        (
            json!({"url":format!("{project_url}#project-update-deadbeef")}),
            "INVALID_LINK",
        ),
    ] {
        let tool = if arguments.get("project_id").is_some() {
            "get_overview"
        } else {
            "get_context"
        };
        let rejected = f.call(tool, arguments).await;
        assert_eq!(rejected.status, "blocked", "{tool}: {}", rejected.data);
        assert_eq!(rejected.data["code"], code, "{tool}: {}", rejected.data);
    }

    let shared_prefix = "abcdef01";
    for suffix in ["1111-4111-8111-000000000001", "2222-4222-8222-000000000002"] {
        f.ok(
            "save_project_update",
            json!({"request_id":format!("{shared_prefix}-{suffix}"),"project_id":project,
                "health":"atRisk","reason":"Two updates share a short token"}),
        )
        .await;
    }
    let ambiguous = f
        .call(
            "get_context",
            json!({"url":format!("{project_url}/activity#project-update-{shared_prefix}")}),
        )
        .await;
    assert_eq!(ambiguous.status, "blocked");
    assert_eq!(ambiguous.data["code"], "INVALID_LINK");
}

/// detail=brief returns one compact current slice for either role and for Projects, keeps the
/// recovery payload and discrepancies, honors native archived markers, and routes to full
/// content while omitted or full detail preserves the legacy complete response.
#[tokio::test]
async fn brief_detail_keeps_current_slice_and_recovery() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let epic = f.work("epic", &project, None).await;
    let module = f.work("module", &project, Some(&epic)).await;
    f.mv(&epic, "In Progress").await;
    f.mv(&module, "In Progress").await;
    let task = f.work("task", &project, Some(&module)).await;
    f.mv(&task, "In Progress").await;
    f.result("task", &task).await;
    let archived_doc = f
        .ok(
            "save_document",
            json!({"issue_id":epic,"title":"Old plan","content":"Historical"}),
        )
        .await;
    let live_doc = f
        .ok(
            "save_document",
            json!({"issue_id":epic,"title":"Epic guide","content":"Current"}),
        )
        .await;
    f.db.lock()
        .await
        .documents
        .get_mut(archived_doc["id"].as_str().unwrap())
        .unwrap()["archivedAt"] = json!("2026-09-25T00:00:00Z");
    let checkpoint = f
        .ok(
            "add_comment",
            json!({"target_type":"issue","target_id":task,"kind":"handoff",
                "body":"Continue from the renderer"}),
        )
        .await["comment"]
        .clone();

    let brief = f
        .ok(
            "get_context",
            json!({"type":"issue","id":task,"detail":"brief"}),
        )
        .await;
    assert_eq!(brief["detail"], "brief");
    assert_eq!(brief["issue"]["id"], json!(task));
    assert_eq!(brief["issue"]["status"], "In Progress");
    assert_eq!(brief["fields"]["check_result"], "Local scenarios passed");
    assert!(brief["agent_context"].is_null());
    assert_eq!(brief["handoff"]["current"]["id"], checkpoint["id"]);
    let documents = brief["documents"].as_array().unwrap();
    // Brief lists own/ancestor Issue links only: the live ancestor document stays, while
    // archived entries and the whole Project catalogue remain behind the explicit routes.
    assert!(
        documents
            .iter()
            .any(|doc| doc["id"] == live_doc["id"] && doc["archived"] == false)
    );
    assert!(!documents.iter().any(|doc| doc["id"] == archived_doc["id"]));
    let project_doc_ids: Vec<_> = {
        let db = f.db.lock().await;
        db.documents
            .values()
            .filter(|doc| doc["project"]["id"] == json!(project))
            .map(|doc| doc["id"].clone())
            .collect()
    };
    assert!(
        documents
            .iter()
            .all(|doc| !project_doc_ids.contains(&doc["id"]))
    );
    assert!(documents.iter().all(|doc| doc.get("content").is_none()));
    assert_eq!(
        brief["full_context"]["issue"],
        format!("get_context type=issue id={task}")
    );
    assert_eq!(brief["runtime"]["tools"], 25);
    assert!(brief["runtime"]["version"].is_string());
    let reviewer_brief = f
        .ok(
            "get_context",
            json!({"type":"issue","id":task,"view":"reviewer","detail":"brief"}),
        )
        .await;
    assert_eq!(reviewer_brief["handoff"]["current"]["id"], checkpoint["id"]);
    assert!(reviewer_brief["review_evidence"].is_null());

    // A pending write keeps its exact recoverable payload inside the brief slice.
    let pending_request =
        json!({"request_id":id(),"id":task,"fields":{"description":"Pending edit"}});
    f.db.lock().await.lose = Some("MUpdateIssue".into());
    assert_eq!(
        f.call("edit_task", pending_request).await.status,
        "outcome_unknown"
    );
    let pending_brief = f
        .ok(
            "get_context",
            json!({"type":"issue","id":task,"detail":"brief"}),
        )
        .await;
    assert_eq!(
        pending_brief["workflow"]["pending"]["request"]["tool"],
        "edit_task"
    );
    assert!(
        pending_brief["discrepancies"]
            .as_array()
            .unwrap()
            .iter()
            .any(|problem| problem.as_str().unwrap().contains("pending"))
    );

    // Omitted and full detail keep the complete legacy response.
    for detail in [None, Some("full")] {
        let mut arguments = json!({"type":"issue","id":task,"view":"lead"});
        if let Some(detail) = detail {
            arguments["detail"] = json!(detail);
        }
        let full = f.ok("get_context", arguments).await;
        assert!(full["agent_context"].is_object());
        assert!(full["issue"]["description"].is_string());
        assert!(full["handoff"].is_null());
    }

    // Project brief lists document links and archive routes without bodies.
    let project_brief = f
        .ok(
            "get_context",
            json!({"type":"project","id":project,"detail":"brief"}),
        )
        .await;
    assert_eq!(project_brief["detail"], "brief");
    assert!(project_brief["project"]["url"].is_string());
    assert!(
        project_brief["documents"]
            .as_array()
            .unwrap()
            .iter()
            .all(|doc| doc.get("content").is_none())
    );
    assert_eq!(
        project_brief["full_context"]["archive"],
        format!("list_items type=document project_id={project} include_archived=true")
    );

    // Explicit document reads stay complete regardless of detail.
    let document = f
        .ok(
            "save_document",
            json!({"issue_id":task,"title":"Full body","content":"Complete prose"}),
        )
        .await;
    let full_document = f
        .ok(
            "get_context",
            json!({"type":"document","id":document["id"],"detail":"brief"}),
        )
        .await;
    assert_eq!(full_document["content"], "Complete prose");
}

/// A child with uncertain native Done and pending recorded transition is never counted as exact.
#[tokio::test]
async fn pending_child_done_suppresses_module_progress() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let module = f.work("module", &project, None).await;
    f.mv(&module, "In Progress").await;
    let task = f.work("task", &project, Some(&module)).await;
    f.mv(&task, "In Progress").await;
    f.result("task", &task).await;
    f.db.lock().await.lose = Some("MUpdateIssue".into());
    let lost = f
        .call(
            "move_status",
            json!({"id":task,"status":"Done","actor_role":"worker"}),
        )
        .await;
    assert_eq!(lost.status, "outcome_unknown");
    let context = f
        .ok(
            "get_context",
            json!({"type":"issue","id":module,"view":"lead"}),
        )
        .await;
    assert!(context["module_report"].is_null());
    assert!(
        context["discrepancies"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry.as_str().unwrap().contains("pending"))
    );
    assert!(
        context["agent_context"]["discrepancies"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry.as_str().unwrap().contains("Native status differs"))
    );
    let overview = f.call("get_overview", json!({"project_id":project})).await;
    assert_eq!(overview.data["code"], "INCOMPLETE_DATA");
}

/// Overview attention comes from the same guidance helper as reads and ACKs: awaiting review,
/// an accepted review without a reported merge, and a pending write each surface their stage,
/// responsible role and next tool without masking incomplete data.
#[tokio::test]
async fn overview_attention_lists_unfinished_actions() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let epic = f.work("epic", &project, None).await;
    let module = f.work("module", &project, Some(&epic)).await;
    f.mv(&epic, "In Progress").await;
    f.mv(&module, "In Progress").await;
    f.result("module", &module).await;
    f.mv(&module, "In Review").await;

    let stage_of = |overview: &serde_json::Value, id: &str| {
        overview["attention"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == json!(id))
            .map(|entry| entry["stage"].as_str().unwrap().to_owned())
    };
    let awaiting = f.ok("get_overview", json!({"project_id":project})).await;
    assert_eq!(stage_of(&awaiting, &module), Some("review".to_owned()));
    let entry = awaiting["attention"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == json!(module))
        .unwrap();
    assert_eq!(entry["next_action"]["tool"], "record_review");
    assert_eq!(entry["next_action"]["actor_role"], "reviewer");

    f.review(&module, "accepted").await;
    let merging = f.ok("get_overview", json!({"project_id":project})).await;
    assert_eq!(stage_of(&merging, &module), Some("merge".to_owned()));

    f.ok(
        "edit_module",
        json!({"id":module,"fields":{"merge_report":"PR merged into main"}}),
    )
    .await;
    f.mv(&module, "Done").await;
    let closed = f.ok("get_overview", json!({"project_id":project})).await;
    assert_eq!(stage_of(&closed, &module), None);

    let atomic = f.work("atomic", &project, Some(&epic)).await;
    f.mv(&atomic, "In Progress").await;
    f.db.lock().await.lose = Some("MUpdateIssue".into());
    assert_eq!(
        f.call(
            "edit_atomic",
            json!({"id":atomic,"fields":{"description":"Uncertain write"}}),
        )
        .await
        .status,
        "outcome_unknown"
    );
    let recovering = f.ok("get_overview", json!({"project_id":project})).await;
    let attention = stage_of(&recovering, &atomic);
    assert_eq!(attention, Some("recovery".to_owned()));
    let entry = recovering["attention"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == json!(atomic))
        .unwrap();
    assert_eq!(entry["next_action"]["kind"], "retry_operation");
    assert_eq!(entry["next_action"]["tool"], "edit_atomic");
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
