//! End-to-end guarded workflow tests using native-shaped Linear HTTP responses.
mod support;
use serde_json::json;
use support::{Fixture, id};

/// Exercise two modules, local task completion, module review, merge, integration and epic closure.
#[tokio::test]
async fn complete_cycle_freezes_epic_and_requires_current_integration() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let epic = f.work("epic", &project, None).await;
    let m1 = f.work("module", &project, Some(&epic)).await;
    let m2 = f.work("module", &project, Some(&epic)).await;
    let task = f.work("task", &project, Some(&m1)).await;
    let early = f
        .call(
            "move_status",
            json!({"id":m1,"status":"In Progress","actor_role":"orchestrator"}),
        )
        .await;
    assert_eq!(early.status, "blocked");
    f.mv(&epic, "In Progress").await;
    let late = f
        .call(
            "create_module",
            json!({"project_id":project,"team_id":f.team,"parent_id":epic,"title":"Late scope"}),
        )
        .await;
    assert_eq!(late.status, "blocked");
    f.mv(&m1, "In Progress").await;
    f.mv(&m2, "In Progress").await;
    f.mv(&task, "In Progress").await;
    assert_eq!(
        f.call(
            "move_status",
            json!({"id":task,"status":"In Review","actor_role":"worker"})
        )
        .await
        .status,
        "blocked"
    );
    f.result("task", &task).await;
    f.mv(&task, "Done").await;
    f.finish_module(&m1).await;
    f.finish_module(&m2).await;
    f.result("epic", &epic).await;
    assert_eq!(
        f.call(
            "move_status",
            json!({"id":epic,"status":"In Review","actor_role":"orchestrator"})
        )
        .await
        .status,
        "blocked"
    );
    let seam = f.work("atomic", &project, Some(&epic)).await;
    f.ok("edit_atomic",json!({"id":seam,"fields":{"work_type":"integration","integration_modules":[m1,m2],"scenarios":"Request traverses both Modules","environment":"combined checkout"}})).await;
    f.mv(&seam, "In Progress").await;
    f.result("atomic", &seam).await;
    f.mv(&seam, "In Review").await;
    f.review(&seam, "accepted").await;
    f.mv(&seam, "Done").await;
    f.mv(&epic, "In Review").await;
    f.review(&epic, "accepted").await;
    f.mv(&epic, "Done").await;
    f.mv(&epic, "In Progress").await;
    assert_eq!(f.call("create_module",json!({"project_id":project,"team_id":f.team,"parent_id":epic,"title":"Still forbidden"})).await.status,"blocked");
    f.mv(&m1, "In Progress").await;
    f.finish_module(&m1).await;
    f.result("epic", &epic).await;
    assert_eq!(
        f.call(
            "move_status",
            json!({"id":epic,"status":"In Review","actor_role":"orchestrator"})
        )
        .await
        .status,
        "blocked"
    );
    f.mv(&seam, "In Progress").await;
    assert_eq!(
        f.call(
            "move_status",
            json!({"id":seam,"status":"In Review","actor_role":"orchestrator"})
        )
        .await
        .status,
        "blocked"
    );
    f.result("atomic", &seam).await;
    f.mv(&seam, "In Review").await;
    f.review(&seam, "accepted").await;
    f.mv(&seam, "Done").await;
    f.mv(&epic, "In Review").await;
}

/// Test independent/waiting modules, non-code work, corrections, retirement and frozen parent changes.
#[tokio::test]
async fn independent_queue_review_corrections_and_cancellation() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let epic = f.work("epic", &project, None).await;
    let module = f.work("module", &project, None).await;
    assert_eq!(f.db.lock().await.issues[&module]["state"]["name"], "Todo");
    f.ok(
        "edit_module",
        json!({"id":module,"fields":{"after_epic":epic}}),
    )
    .await;
    assert_eq!(
        f.call(
            "move_status",
            json!({"id":module,"status":"In Progress","actor_role":"orchestrator"})
        )
        .await
        .status,
        "blocked"
    );
    f.mv(&epic, "In Progress").await;
    f.result("epic", &epic).await;
    f.mv(&epic, "In Review").await;
    f.review(&epic, "accepted").await;
    f.mv(&epic, "Done").await;
    f.mv(&module, "In Progress").await;
    f.result("module", &module).await;
    f.mv(&module, "In Review").await;
    f.review(&module, "changes_requested").await;
    assert_eq!(
        f.call(
            "move_status",
            json!({"id":module,"status":"Done","actor_role":"orchestrator"})
        )
        .await
        .status,
        "blocked"
    );
    f.mv(&module, "In Progress").await;
    assert_eq!(
        f.call(
            "move_status",
            json!({"id":module,"status":"In Review","actor_role":"orchestrator"})
        )
        .await
        .status,
        "blocked"
    );
    f.finish_module(&module).await;
    assert_eq!(f.db.lock().await.comments.len(), 3);
    let independent = f.work("module", &project, None).await;
    f.mv(&independent, "In Progress").await;
    let child = f.work("atomic", &project, Some(&independent)).await;
    f.ok(
        "edit_module",
        json!({"id":independent,"fields":{"reason":"Scope dropped"}}),
    )
    .await;
    assert_eq!(
        f.call(
            "move_status",
            json!({"id":independent,"status":"Canceled","actor_role":"orchestrator"})
        )
        .await
        .status,
        "blocked"
    );
    f.ok("edit_atomic",json!({"id":child,"fields":{"reason":"Duplicate request","duplicate_of":"https://linear.app/issue/original"}})).await;
    f.mv(&child, "Duplicate").await;
    f.mv(&independent, "Canceled").await;
    let invalid=f.call("create_atomic",json!({"project_id":project,"team_id":f.team,"parent_id":child,"title":"Invalid nesting"})).await;
    assert_eq!(invalid.status, "blocked");
}

/// Real transport errors preserve a prepared operation; retry after a cold start creates no duplicates.
#[tokio::test]
async fn retries_cold_start_manual_drift_and_partial_edits() {
    let mut f = Fixture::new().await;
    let project = f.project().await;
    let epic = f.work("epic", &project, None).await;
    let module = f.work("module", &project, Some(&epic)).await;
    let request =
        json!({"request_id":id(),"id":epic,"status":"In Progress","actor_role":"orchestrator"});
    f.db.lock().await.lose = Some("MUpdateIssue".into());
    let lost = f.call("move_status", request.clone()).await;
    assert_eq!(lost.status, "outcome_unknown");
    f.restart();
    let c = f.ok("get_context", json!({"type":"issue","id":epic})).await;
    assert!(
        c["discrepancies"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str().unwrap().contains("pending"))
    );
    f.ok("move_status", request.clone()).await;
    f.ok("move_status", request).await;
    assert_eq!(f.db.lock().await.issues.len(), 2);
    {
        let mut db = f.db.lock().await;
        db.issues.get_mut(&module).unwrap()["parent"] = serde_json::Value::Null;
    }
    let drift = f.ok("get_context", json!({"type":"issue","id":epic})).await;
    assert!(
        drift["discrepancies"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str().unwrap().contains("membership"))
    );
    assert_eq!(
        f.call(
            "move_status",
            json!({"id":epic,"status":"In Review","actor_role":"orchestrator"})
        )
        .await
        .status,
        "blocked"
    );
    assert!(f.db.lock().await.issues[&module]["parent"].is_null());
    {
        let mut db = f.db.lock().await;
        db.issues.get_mut(&module).unwrap()["parent"] = json!({"id":epic});
        let d = db.issues[&module]["description"]
            .as_str()
            .unwrap()
            .to_string();
        db.issues.get_mut(&module).unwrap()["description"] =
            json!(format!("{d}\n## User notes\nPreserve this exactly\n"));
    }
    f.ok(
        "edit_module",
        json!({"id":module,"fields":{"branch":"feature/updated"}}),
    )
    .await;
    assert!(
        f.db.lock().await.issues[&module]["description"]
            .as_str()
            .unwrap()
            .contains("## User notes\nPreserve this exactly\n")
    );
    let before = f.db.lock().await.attachments.clone();
    f.ok(
        "move_status",
        json!({"id":module,"status":"In Progress","actor_role":"worker","check_only":true}),
    )
    .await;
    assert_eq!(before, f.db.lock().await.attachments);
    f.mv(&module, "In Progress").await;
}

/// Guard manual task moves across Projects and preserve edits during ambiguous-write recovery.
#[tokio::test]
async fn detached_tasks_and_pending_manual_edits_block_without_overwriting() {
    let mut f = Fixture::new().await;
    let project = f.project().await;
    let other = f.project().await;
    let module = f.work("module", &project, None).await;
    let task = f.work("task", &project, Some(&module)).await;
    f.mv(&module, "In Progress").await;
    f.result("module", &module).await;
    {
        let mut db = f.db.lock().await;
        db.issues.get_mut(&task).unwrap()["parent"] = serde_json::Value::Null;
        db.issues.get_mut(&task).unwrap()["project"]["id"] = json!(other);
    }
    let drift = f
        .ok("get_context", json!({"type":"issue","id":module}))
        .await;
    assert!(
        drift["discrepancies"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e.as_str().unwrap().contains("child"))
    );
    assert_eq!(
        f.call(
            "move_status",
            json!({"id":module,"status":"In Review","actor_role":"orchestrator"})
        )
        .await
        .status,
        "blocked"
    );
    {
        let mut db = f.db.lock().await;
        db.issues.get_mut(&task).unwrap()["parent"] = json!({"id":module});
        db.issues.get_mut(&task).unwrap()["project"]["id"] = json!(project);
    }
    let edit = json!({"request_id":id(),"id":task,"fields":{"result":"Prepared result"}});
    f.db.lock().await.lose = Some("MUpdateIssue".into());
    assert_eq!(
        f.call("edit_task", edit.clone()).await.status,
        "outcome_unknown"
    );
    let confirmed = f.db.lock().await.issues[&task]["description"].clone();
    let manual = format!(
        "{}\n\n## Owner notes\nDo not erase this text\n",
        confirmed.as_str().unwrap()
    );
    f.db.lock().await.issues.get_mut(&task).unwrap()["description"] = json!(manual);
    f.restart();
    assert_eq!(f.call("edit_task", edit.clone()).await.status, "blocked");
    assert_eq!(f.db.lock().await.issues[&task]["description"], manual);
    // The human resolves the conflicting edit explicitly; MCP can now finalize the already applied write.
    f.db.lock().await.issues.get_mut(&task).unwrap()["description"] = confirmed;
    f.ok("edit_task", edit).await;
}

/// Validate manual field adoption, native Markdown escaping, code artifacts and Project-level integration.
#[tokio::test]
async fn field_validation_code_work_and_project_integration() {
    use agent_tasks_linear::records::{markdown_key, read_fields};
    let a = id();
    let b = id();
    let description = format!("## Проверяемые модули\n\n\\[\"{a}\",\"{b}\"\\]\n");
    assert_eq!(
        read_fields(&description).unwrap()["integration_modules"],
        json!([a, b])
    );
    let url = "https://example.com/report";
    assert_eq!(
        markdown_key(&format!("[]\n{url}")),
        markdown_key(&format!("[]\n[{url}](<{url}>)"))
    );
    assert_ne!(
        markdown_key(url),
        markdown_key(&format!("[Different label](<{url}>)"))
    );
    assert_eq!(
        read_fields(&format!("## Артефакт\n\n[Report](<{url}>)")).unwrap()["artifact_url"],
        url
    );
    assert_eq!(
        markdown_key(&format!("## Артефакт\n{url}")),
        markdown_key(&format!("## Артефакт\n\n[Report](<{url}>)"))
    );
    assert_ne!(
        markdown_key(&format!("## Артефакт\n{url}")),
        markdown_key(&format!("## Артефакт\n{url}\n\n## Notes\nPreserve me"))
    );
    let paren = "https://example.com/a(b)c";
    assert_eq!(
        markdown_key(paren),
        markdown_key(&format!("[{paren}](<{paren}>)"))
    );
    assert_eq!(
        markdown_key(paren),
        markdown_key(&format!("[{paren}]({paren})"))
    );
    let f = Fixture::new().await;
    let project = f.project().await;
    let one = f.work("module", &project, None).await;
    let two = f.work("module", &project, None).await;
    assert_eq!(
        f.call(
            "move_status",
            json!({"id":one,"status":"In Progress","actor_role":"worker"})
        )
        .await
        .status,
        "blocked"
    );
    f.mv(&one, "In Progress").await;
    f.mv(&two, "In Progress").await;
    let task = f.work("task", &project, Some(&one)).await;
    f.ok(
        "edit_task",
        json!({"id":task,"fields":{"work_type":"code"}}),
    )
    .await;
    f.mv(&task, "In Progress").await;
    f.result("task", &task).await;
    assert_eq!(
        f.call(
            "move_status",
            json!({"id":task,"status":"Done","actor_role":"worker"})
        )
        .await
        .status,
        "blocked"
    );
    f.ok(
        "edit_task",
        json!({"id":task,"fields":{"commit_url":"https://example.com/commit"}}),
    )
    .await;
    f.mv(&task, "Done").await;
    f.finish_module(&one).await;
    f.finish_module(&two).await;
    let atom = f.work("atomic", &project, None).await;
    f.ok("edit_atomic",json!({"id":atom,"fields":{"work_type":"code","repository_url":"https://github.com/example/product","branch":"feature/atomic","worktree":"/tmp/atomic"}})).await;
    f.mv(&atom, "In Progress").await;
    f.result("atomic", &atom).await;
    assert_eq!(
        f.call(
            "move_status",
            json!({"id":atom,"status":"In Review","actor_role":"orchestrator"})
        )
        .await
        .status,
        "blocked"
    );
    f.ok(
        "edit_atomic",
        json!({"id":atom,"fields":{"commit_url":"https://example.com/commit"}}),
    )
    .await;
    f.mv(&atom, "In Review").await;
    f.review(&atom, "accepted").await;
    f.mv(&atom, "Done").await;
    let seam = f.work("atomic", &project, None).await;
    f.ok("edit_atomic",json!({"id":seam,"fields":{"work_type":"integration","integration_modules":[one,two],"scenarios":"Combined request","environment":"Integration checkout"}})).await;
    let good = f.db.lock().await.issues[&seam]["description"].clone();
    let bad = good
        .as_str()
        .unwrap()
        .replace(&format!("\"{two}\"]"), &format!("\"{two}\",42]"));
    f.db.lock().await.issues.get_mut(&seam).unwrap()["description"] = json!(bad);
    assert_eq!(
        f.call(
            "edit_atomic",
            json!({"id":seam,"fields":{"scope":"Unrelated change"}})
        )
        .await
        .status,
        "blocked"
    );
    f.db.lock().await.issues.get_mut(&seam).unwrap()["description"] = good;
    f.mv(&seam, "In Progress").await;
    f.mv(&one, "In Progress").await;
    f.finish_module(&one).await;
    f.mv(&seam, "In Progress").await;
    let context = f.ok("get_context", json!({"type":"issue","id":seam})).await;
    assert_eq!(context["workflow"]["round"], 2);
    f.result("atomic", &seam).await;
    let original = f.db.lock().await.issues[&one]["description"].clone();
    f.db.lock().await.issues.get_mut(&one).unwrap()["description"] = json!(format!(
        "{}\n\n## Manual change\nNew module behavior",
        original.as_str().unwrap()
    ));
    assert_eq!(
        f.call(
            "move_status",
            json!({"id":seam,"status":"In Review","actor_role":"orchestrator"})
        )
        .await
        .status,
        "blocked"
    );
    f.db.lock().await.issues.get_mut(&one).unwrap()["description"] = original.clone();
    f.mv(&seam, "In Review").await;
    f.review(&seam, "accepted").await;
    f.db.lock().await.issues.get_mut(&one).unwrap()["description"] = json!(format!(
        "{}\n\n## Manual change\nAfter review",
        original.as_str().unwrap()
    ));
    assert_eq!(
        f.call(
            "move_status",
            json!({"id":seam,"status":"Done","actor_role":"orchestrator"})
        )
        .await
        .status,
        "blocked"
    );
    f.db.lock().await.issues.get_mut(&one).unwrap()["description"] = original;
    f.mv(&seam, "Done").await;
}

/// Retry uncertain creates/reviews by reserved IDs and prohibit edit-based changes to frozen membership.
#[tokio::test]
async fn uncertain_creates_reviews_and_frozen_reparenting() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let epic = f.work("epic", &project, None).await;
    let create = json!({"request_id":id(),"project_id":project,"team_id":f.team,"parent_id":epic,"title":"Retry-safe module"});
    f.db.lock().await.lose = Some("MCreateIssue".into());
    assert_eq!(
        f.call("create_module", create.clone()).await.status,
        "outcome_unknown"
    );
    let created = f.ok("create_module", create.clone()).await;
    f.ok("create_module", create).await;
    let module = created["issue"]["id"].as_str().unwrap();
    assert_eq!(f.db.lock().await.issues.len(), 2);
    f.mv(&epic, "In Progress").await;
    assert_eq!(
        f.call("edit_module", json!({"id":module,"parent_id":null}))
            .await
            .status,
        "blocked"
    );
    let standalone = f.work("module", &project, None).await;
    assert_eq!(
        f.call("edit_module", json!({"id":standalone,"parent_id":epic}))
            .await
            .status,
        "blocked"
    );
    let doc = json!({"request_id":id(),"project_id":project,"title":"Retry document","content":"Keep this content"});
    f.db.lock().await.lose = Some("MCreateDocument".into());
    assert_eq!(
        f.call("save_document", doc.clone()).await.status,
        "outcome_unknown"
    );
    f.ok("save_document", doc.clone()).await;
    f.ok("save_document", doc).await;
    assert_eq!(f.db.lock().await.documents.len(), 3);
    let atom = f.work("atomic", &project, None).await;
    f.mv(&atom, "In Progress").await;
    f.result("atomic", &atom).await;
    f.mv(&atom, "In Review").await;
    let review = json!({"request_id":id(),"id":atom,"reviewer":"codex:reviewer","verdict":"accepted","summary":"Checked result","findings":"","artifacts":["https://example.com/report"]});
    f.db.lock().await.lose = Some("MCreateComment".into());
    assert_eq!(
        f.call("record_review", review.clone()).await.status,
        "outcome_unknown"
    );
    f.ok("record_review", review.clone()).await;
    f.ok("record_review", review.clone()).await;
    assert_eq!(f.db.lock().await.comments.len(), 1);
    f.mv(&atom, "In Progress").await;
    f.result("atomic", &atom).await;
    f.mv(&atom, "In Review").await;
    assert_eq!(f.call("record_review", review).await.status, "blocked");
}

/// A successful native envelope with unapplied fields never publishes a false workflow result.
#[tokio::test]
async fn stale_success_payload_is_unknown_until_fields_are_confirmed() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let atom = f.work("atomic", &project, None).await;
    let request = json!({"request_id":id(),"id":atom,"fields":{"result":"Visible result"}});
    f.db.lock().await.stale_update = true;
    let outcome = f.call("edit_atomic", request.clone()).await;
    assert_eq!(outcome.status, "outcome_unknown");
    let context = f.ok("get_context", json!({"type":"issue","id":atom})).await;
    assert!(context["fields"]["result"].is_null());
    assert!(context["workflow"]["pending"].is_object());
    f.ok("edit_atomic", request).await;
    let context = f.ok("get_context", json!({"type":"issue","id":atom})).await;
    assert_eq!(context["fields"]["result"], "Visible result");
}

/// Verify canonical issue titles, full-group priority pagination and identity-safe presentation edits.
#[tokio::test]
async fn canonical_titles_and_priority_views_preserve_workflow_identity() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let _epic = f.work("epic", &project, None).await;
    let module = f.work("module", &project, None).await;
    let wrong_parent = id();
    let wrong_project = id();
    let wrong_kind = id();
    let mut tied_ids = Vec::new();
    {
        let mut db = f.db.lock().await;
        let base = db.issues[&module].clone();
        assert_eq!(base["title"], "[MODULE] Readable module");
        db.issues.get_mut(&module).unwrap()["priority"] = json!(2);
        for i in 0..105 {
            let mut item = base.clone();
            let issue_id = id();
            item["id"] = json!(issue_id);
            item["title"] = json!(format!("[MODULE] sibling {i}"));
            item["priority"] = json!(if i % 3 == 0 { 1 } else { 0 });
            item["prioritySortOrder"] = json!(if i == 1 || i == 4 { -49 } else { i as i64 - 50 });
            if i == 1 || i == 4 {
                tied_ids.push(issue_id.clone());
            }
            db.issues.insert(issue_id, item);
        }
        let mut moved = base.clone();
        moved["id"] = json!(wrong_parent);
        moved["parent"] = json!({"id":id()});
        db.issues.insert(wrong_parent.clone(), moved);
        let mut other_project = base.clone();
        other_project["id"] = json!(wrong_project);
        other_project["project"]["id"] = json!(id());
        db.issues.insert(wrong_project.clone(), other_project);
        let mut other_kind = base.clone();
        other_kind["id"] = json!(wrong_kind);
        let epic_label = db
            .labels
            .values()
            .find(|v| v["name"] == "EPIC")
            .unwrap()
            .clone();
        other_kind["labels"] =
            json!({"nodes":[epic_label],"pageInfo":{"hasNextPage":false,"endCursor":null}});
        db.issues.insert(wrong_kind.clone(), other_kind);
    }
    let page1 = f.ok("list_items",json!({"type":"issue","project_id":project,"kind":"module","order_by":"priority","first":100})).await;
    assert_eq!(page1["nodes"].as_array().unwrap().len(), 100);
    let cursor = page1["pageInfo"]["endCursor"].as_str().unwrap().to_owned();
    let page2 = f.ok("list_items",json!({"type":"issue","project_id":project,"kind":"module","order_by":"priority","first":100,"after":cursor})).await;
    assert_eq!(page2["nodes"].as_array().unwrap().len(), 6);
    let ordered: Vec<_> = page1["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .chain(page2["nodes"].as_array().unwrap())
        .collect();
    assert_eq!(ordered.len(), 106);
    assert!(ordered[..35].iter().all(|v| v["priority"] == 1));
    assert_eq!(ordered[35]["priority"], 2);
    assert!(ordered[36..].iter().all(|v| v["priority"] == 0));
    for pair in ordered.windows(2) {
        let priority_rank = |v: &serde_json::Value| {
            if v["priority"] == 0 {
                5
            } else {
                v["priority"].as_u64().unwrap_or(5)
            }
        };
        assert!(priority_rank(pair[0]) <= priority_rank(pair[1]));
        if priority_rank(pair[0]) == priority_rank(pair[1]) {
            assert!(
                pair[0]["prioritySortOrder"].as_f64().unwrap()
                    <= pair[1]["prioritySortOrder"].as_f64().unwrap()
            );
        }
    }
    let mut expected_ties = tied_ids.clone();
    expected_ties.sort();
    let actual_ties: Vec<_> = ordered
        .iter()
        .filter(|v| v["priority"] == 0 && v["prioritySortOrder"] == -49)
        .map(|v| v["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(actual_ties, expected_ties);
    let priority_filter = f
        .ok(
            "list_items",
            json!({"type":"issue","project_id":project,"priority":1}),
        )
        .await;
    assert_eq!(priority_filter["nodes"].as_array().unwrap().len(), 35);
    assert_eq!(
        f.call("list_items", json!({"type":"project","priority":1}))
            .await
            .status,
        "blocked"
    );
    let mismatch = f.call("list_items",json!({"type":"issue","project_id":project,"kind":"module","order_by":"priority","status":"Done","after":cursor})).await;
    assert_eq!(mismatch.status, "blocked");
    let invalid_parent = f.call("list_items",json!({"type":"issue","project_id":project,"kind":"module","order_by":"priority","parent_id":id()})).await;
    assert_eq!(invalid_parent.status, "blocked");
    let anchor = serde_json::from_str::<serde_json::Value>(&cursor).unwrap()["last"]
        .as_str()
        .unwrap()
        .to_owned();
    f.db.lock().await.issues.remove(&anchor);
    let missing_anchor = f.call("list_items",json!({"type":"issue","project_id":project,"kind":"module","order_by":"priority","first":100,"after":cursor})).await;
    assert_eq!(missing_anchor.status, "blocked");

    let created = f.ok("create_epic",json!({"project_id":project,"team_id":f.team,"title":"[TASK] [epic] [EPIC] [UI] Ship it"})).await;
    assert_eq!(created["issue"]["title"], "[EPIC] [UI] Ship it");
    assert_eq!(created["issue"]["priority"], 0);
    let epic_id = created["issue"]["id"].as_str().unwrap();
    f.ok("edit_epic", json!({"id":epic_id,"fields":{}})).await;
    assert_eq!(
        f.db.lock().await.issues[epic_id]["title"],
        "[EPIC] [UI] Ship it"
    );
    let invalid_priority = f
        .call(
            "create_epic",
            json!({"project_id":project,"team_id":f.team,"title":"Bad priority","priority":5}),
        )
        .await;
    assert_eq!(invalid_priority.status, "blocked");
    let blank = f
        .call(
            "create_epic",
            json!({"project_id":project,"team_id":f.team,"title":"[TASK] [EPIC] "}),
        )
        .await;
    assert_eq!(blank.status, "blocked");

    f.mv(&module, "In Progress").await;
    f.result("module", &module).await;
    f.mv(&module, "In Review").await;
    f.review(&module, "accepted").await;
    f.ok(
        "edit_module",
        json!({"id":module,"fields":{"merge_report":"Merged"}}),
    )
    .await;
    f.mv(&module, "Done").await;
    let before = f
        .ok("get_context", json!({"type":"issue","id":module}))
        .await;
    let desc = before["issue"]["description"].clone();
    f.ok(
        "edit_module",
        json!({"id":module,"title":"[TASK] [MODULE] Revised","priority":4}),
    )
    .await;
    let after = f
        .ok("get_context", json!({"type":"issue","id":module}))
        .await;
    assert_eq!(after["issue"]["title"], "[MODULE] Revised");
    assert_eq!(after["issue"]["priority"], 4);
    assert_eq!(after["issue"]["description"], desc);
    assert_eq!(after["workflow"]["status"], "Done");
    assert_eq!(
        after["workflow"]["revision"],
        before["workflow"]["revision"]
    );
    assert_eq!(after["workflow"]["review"], before["workflow"]["review"]);
    assert_eq!(after["fields"]["result"], before["fields"]["result"]);
    {
        let mut db = f.db.lock().await;
        db.issues.get_mut(&module).unwrap()["title"] = json!("[TASK] [EPIC] Existing");
        db.issues.get_mut(&module).unwrap()["priority"] = json!(3);
    }
    f.ok("edit_module", json!({"id":module,"fields":{}})).await;
    let normalized = f
        .ok("get_context", json!({"type":"issue","id":module}))
        .await;
    assert_eq!(normalized["issue"]["title"], "[MODULE] Existing");
    assert_eq!(normalized["issue"]["priority"], 3);
    assert_eq!(normalized["workflow"]["status"], "Done");
    assert_eq!(
        normalized["workflow"]["review"],
        before["workflow"]["review"]
    );
    f.ok("edit_module", json!({"id":module,"priority":0})).await;
    assert_eq!(
        f.ok("get_context", json!({"type":"issue","id":module}))
            .await["issue"]["priority"],
        0
    );
    let drifted = json!(format!("{}\nmanual note", desc.as_str().unwrap()));
    f.db.lock().await.issues.get_mut(&module).unwrap()["description"] = drifted.clone();
    f.ok(
        "edit_module",
        json!({"id":module,"parent_id":null,"fields":{}}),
    )
    .await;
    let after_drift = f
        .ok("get_context", json!({"type":"issue","id":module}))
        .await;
    assert_eq!(after_drift["issue"]["description"], drifted);
    assert_eq!(after_drift["workflow"]["description"], desc);
    assert_eq!(
        after_drift["workflow"]["review"],
        before["workflow"]["review"]
    );
    assert!(after["priority_group"]["peers"].as_array().unwrap().len() >= 100);
    let peer_ids: Vec<_> = after["priority_group"]["peers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["id"].as_str().unwrap())
        .collect();
    assert!(!peer_ids.contains(&wrong_parent.as_str()));
    assert!(!peer_ids.contains(&wrong_project.as_str()));
    assert!(!peer_ids.contains(&wrong_kind.as_str()));
}
