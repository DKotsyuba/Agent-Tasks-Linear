//! Guarded native Document write contract against the native HTTP fixture: precondition,
//! no-op replay, neighbour preservation, hide/unhide, parent rebinding and exact retry after
//! a lost reply or a cold restart; plus scoped Document search with honest snippets, currentness
//! and filtered-page accounting.
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

/// Body-phrase and title hits report an honest, explainable match source and a bounded snippet;
/// the full source stays reachable by id/url rather than being replaced by the snippet.
#[tokio::test]
async fn document_search_finds_body_phrase_with_honest_snippet() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let module = f.work("module", &project, None).await;
    let content = "Intro text.\n\nThe quick brown fox jumps over the lazy dog.\n\nMore text.";
    let document = f
        .ok(
            "save_document",
            json!({"issue_id":module,"title":"Distinctive Runbook Title","content":content}),
        )
        .await;

    let body_hit = f
        .ok("search", json!({"type":"document","query":"brown fox"}))
        .await;
    let nodes = body_hit["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0]["id"], document["id"]);
    assert_eq!(nodes[0]["match_source"], "content");
    assert!(nodes[0]["snippet"].as_str().unwrap().contains("brown fox"));
    assert_eq!(nodes[0]["url"], document["url"]);
    assert_eq!(nodes[0]["current"], true);
    assert_eq!(nodes[0]["archived"], false);
    assert_eq!(nodes[0]["hidden"], false);

    let title_hit = f
        .ok(
            "search",
            json!({"type":"document","query":"Distinctive Runbook"}),
        )
        .await;
    let title_nodes = title_hit["nodes"].as_array().unwrap();
    assert_eq!(title_nodes.len(), 1);
    assert_eq!(title_nodes[0]["match_source"], "title");
}

/// Document search scopes to one Project, including Documents attached to Issues in that
/// Project but excluding Documents elsewhere; the unscoped search still finds everything, and a
/// project-filtered page reports honestly how many native results it actually matched.
#[tokio::test]
async fn document_search_scopes_to_project_including_issue_attached_documents() {
    let f = Fixture::new().await;
    let project_a = f.project().await;
    let project_b = f.project().await;
    let module_a = f.work("module", &project_a, None).await;
    let module_b = f.work("module", &project_b, None).await;

    let on_project_a = f
        .ok(
            "save_document",
            json!({"project_id":project_a,"title":"A project doc","content":"shared keyword here"}),
        )
        .await;
    let on_issue_in_a = f
        .ok(
            "save_document",
            json!({"issue_id":module_a,"title":"An issue doc in A","content":"shared keyword here"}),
        )
        .await;
    f.ok(
        "save_document",
        json!({"issue_id":module_b,"title":"An issue doc in B","content":"shared keyword here"}),
    )
    .await;

    let scoped = f
        .ok(
            "search",
            json!({"type":"document","query":"shared keyword","project_id":project_a}),
        )
        .await;
    let scoped_ids: Vec<&str> = scoped["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(scoped_ids.len(), 2);
    assert!(scoped_ids.contains(&on_project_a["id"].as_str().unwrap()));
    assert!(scoped_ids.contains(&on_issue_in_a["id"].as_str().unwrap()));
    assert_eq!(scoped["scoped_to_project"], project_a);
    assert_eq!(scoped["native_page_size"], 3);
    assert_eq!(scoped["matched_in_page"], 2);

    let unscoped = f
        .ok(
            "search",
            json!({"type":"document","query":"shared keyword"}),
        )
        .await;
    assert_eq!(unscoped["nodes"].as_array().unwrap().len(), 3);
    assert!(unscoped.get("scoped_to_project").is_none());
}

/// A hidden Document is excluded from default search and included only with include_archived,
/// even though native `includeArchived` alone does not govern hidden visibility.
#[tokio::test]
async fn document_search_excludes_hidden_by_default() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let module = f.work("module", &project, None).await;
    let document = f
        .ok(
            "save_document",
            json!({"issue_id":module,"title":"Hideable","content":"unique searchable phrase"}),
        )
        .await;
    f.ok(
        "save_document",
        json!({
            "id":document["id"],
            "hidden":true,
            "expected_updated_at":document["updatedAt"],
        }),
    )
    .await;

    let default_search = f
        .ok(
            "search",
            json!({"type":"document","query":"unique searchable phrase"}),
        )
        .await;
    assert_eq!(default_search["nodes"].as_array().unwrap().len(), 0);
    assert_eq!(default_search["native_page_size"], 1);
    assert_eq!(default_search["matched_in_page"], 0);

    let with_archived = f
        .ok(
            "search",
            json!({"type":"document","query":"unique searchable phrase","include_archived":true}),
        )
        .await;
    let nodes = with_archived["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0]["hidden"], true);
    assert_eq!(nodes[0]["current"], false);
}
