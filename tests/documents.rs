//! Guarded native Document write contract against the native HTTP fixture: precondition,
//! no-op replay, neighbour preservation, hide/unhide, parent rebinding and exact retry after
//! a lost reply or a cold restart.
#[allow(dead_code)]
mod support;
use serde_json::json;
use support::Fixture;

/// A section edit requires a fresh precondition for a real change, refuses a stale one,
/// replays a no-op without needing any precondition at all, preserves the neighbouring
/// section untouched, and replays the exact same request with no duplicate write after a
/// lost reply and a cold restart, even though its precondition is by then stale.
#[tokio::test]
async fn document_section_edit_is_guarded_and_replays_after_lost_response() {
    let mut f = Fixture::new().await;
    let project = f.project().await;
    let module = f.work("module", &project, None).await;
    let content = "# Notes\n\n## Runbook\n\nOld steps.\n\n## Decisions\n\nUse Postgres.\n";
    let document = f
        .ok(
            "save_document",
            json!({"issue_id":module,"title":"Ops","content":content}),
        )
        .await;
    let doc_id = document["id"].as_str().unwrap();
    let updated_at = document["updatedAt"].as_str().unwrap().to_owned();

    // Missing precondition for a real change.
    let missing = f
        .call(
            "save_document",
            json!({"id":doc_id,"section":"Runbook","content":"\nNew steps.\n\n"}),
        )
        .await;
    assert_eq!(missing.data["code"], "PRECONDITION_REQUIRED");

    // Stale precondition.
    let stale = f
        .call(
            "save_document",
            json!({"id":doc_id,"section":"Runbook","content":"\nNew steps.\n\n",
                "expected_updated_at":"2020-01-01T00:00:00Z"}),
        )
        .await;
    assert_eq!(stale.data["code"], "PENDING_CONFLICT");

    // Ambiguous/missing headings still fail closed before any write.
    let missing_heading = f
        .call(
            "save_document",
            json!({"id":doc_id,"section":"Absent","content":"x",
                "expected_updated_at":updated_at}),
        )
        .await;
    assert_eq!(missing_heading.data["code"], "SECTION_NOT_FOUND");

    // A request naming the already-current section body is a no-op replay, no precondition
    // needed at all, since nothing would actually change.
    let noop = f
        .ok(
            "save_document",
            json!({"id":doc_id,"section":"Runbook","content":"\nOld steps.\n\n"}),
        )
        .await;
    assert_eq!(noop["replayed"], true);

    // Real write; the neighbouring "Decisions" section is untouched.
    let edited = f
        .ok(
            "save_document",
            json!({"id":doc_id,"section":"Runbook","content":"\nNew steps.\n\n",
                "expected_updated_at":updated_at}),
        )
        .await;
    assert_eq!(edited["replayed"], false);
    let edited_content = edited["content"].as_str().unwrap();
    assert!(edited_content.contains("New steps."));
    assert!(!edited_content.contains("Old steps."));
    assert!(edited_content.contains("Use Postgres."));

    // Exact retry after a lost response and a cold restart: same request, now-stale
    // precondition, replays with no duplicate write.
    f.db.lock().await.lose = Some("MUpdateDocument".into());
    let title_request =
        json!({"id":doc_id,"title":"Ops v2","expected_updated_at":edited["updatedAt"]});
    assert_eq!(
        f.call("save_document", title_request.clone()).await.status,
        "outcome_unknown"
    );
    f.restart();
    let replayed = f.ok("save_document", title_request.clone()).await;
    assert_eq!(replayed["replayed"], true);
    assert_eq!(replayed["title"], "Ops v2");
    let replayed_again = f.ok("save_document", title_request).await;
    assert_eq!(replayed_again["replayed"], true);
}

/// `hidden` maps to native `hiddenAt`, only writes when the state actually changes, and a
/// same-process competing write against a stale precondition is refused rather than silently
/// overwriting a concurrent edit.
#[tokio::test]
async fn document_hidden_lifecycle_and_competing_writers() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let module = f.work("module", &project, None).await;
    let document = f
        .ok(
            "save_document",
            json!({"issue_id":module,"title":"Runbook","content":"Body."}),
        )
        .await;
    let doc_id = document["id"].as_str().unwrap();
    let updated_at = document["updatedAt"].as_str().unwrap().to_owned();

    let hidden = f
        .ok(
            "save_document",
            json!({"id":doc_id,"hidden":true,"expected_updated_at":updated_at}),
        )
        .await;
    assert_eq!(hidden["replayed"], false);
    assert!(!hidden["hiddenAt"].is_null());

    // Requesting the same already-hidden state replays with no write, even without a fresh
    // precondition.
    let already_hidden = f
        .ok("save_document", json!({"id":doc_id,"hidden":true}))
        .await;
    assert_eq!(already_hidden["replayed"], true);

    // Two readers observe the hidden state; one unhides, the other's stale precondition on a
    // genuinely different change (not a no-op) is refused instead of silently overwriting it.
    let hidden_updated_at = hidden["updatedAt"].as_str().unwrap().to_owned();
    let unhidden = f
        .ok(
            "save_document",
            json!({"id":doc_id,"hidden":false,"expected_updated_at":hidden_updated_at.clone()}),
        )
        .await;
    assert!(unhidden["hiddenAt"].is_null());
    let competing = f
        .call(
            "save_document",
            json!({"id":doc_id,"title":"Renamed by the other reader",
                "expected_updated_at":hidden_updated_at}),
        )
        .await;
    assert_eq!(competing.data["code"], "PENDING_CONFLICT");
}

/// Rebinding to a new Project or Issue clears the other native parent explicitly; an
/// unrelated request naming both parents, or naming no field to edit, fails before any write.
#[tokio::test]
async fn document_rebind_clears_the_other_native_parent() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let module_a = f.work("module", &project, None).await;
    let module_b = f.work("module", &project, None).await;
    let document = f
        .ok(
            "save_document",
            json!({"issue_id":module_a,"title":"Doc","content":"Body."}),
        )
        .await;
    let doc_id = document["id"].as_str().unwrap();
    assert_eq!(document["issue"]["id"], module_a);
    assert!(document["project"]["id"].is_null());

    let rebound_to_project = f
        .ok(
            "save_document",
            json!({"id":doc_id,"project_id":project,"expected_updated_at":document["updatedAt"]}),
        )
        .await;
    assert_eq!(rebound_to_project["project"]["id"], project);
    assert!(rebound_to_project["issue"]["id"].is_null());

    let rebound_to_issue = f
        .ok(
            "save_document",
            json!({"id":doc_id,"issue_id":module_b,
                "expected_updated_at":rebound_to_project["updatedAt"]}),
        )
        .await;
    assert_eq!(rebound_to_issue["issue"]["id"], module_b);
    assert!(rebound_to_issue["project"]["id"].is_null());

    // Replaying the same rebind after it already applied is a no-op, even with a now-stale
    // precondition.
    let replayed = f
        .ok(
            "save_document",
            json!({"id":doc_id,"issue_id":module_b,
                "expected_updated_at":rebound_to_project["updatedAt"]}),
        )
        .await;
    assert_eq!(replayed["replayed"], true);

    let both = f
        .call(
            "save_document",
            json!({"id":doc_id,"project_id":project,"issue_id":module_b,
                "expected_updated_at":rebound_to_issue["updatedAt"]}),
        )
        .await;
    assert_eq!(both.data["code"], "INVALID_INPUT");

    let nothing = f.call("save_document", json!({"id":doc_id})).await;
    assert_eq!(nothing.data["code"], "INVALID_INPUT");

    let section_without_content = f
        .call(
            "save_document",
            json!({"id":doc_id,"section":"Doc","expected_updated_at":rebound_to_issue["updatedAt"]}),
        )
        .await;
    assert_eq!(section_without_content.data["code"], "INVALID_INPUT");

    // A new Document has no prior state to guard: section/expected_updated_at are rejected.
    let new_with_section = f
        .call(
            "save_document",
            json!({"issue_id":module_a,"title":"X","content":"Y","section":"X"}),
        )
        .await;
    assert_eq!(new_with_section.data["code"], "INVALID_INPUT");
}
