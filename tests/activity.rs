//! Native comment activity checks through the public tools and HTTP-shaped Linear fixture.
#[allow(dead_code)]
mod support;
use serde_json::json;
use support::{Fixture, id};

/// Comment creation survives a lost reply; native URLs resolve to full IDs, replies remain
/// target-scoped, and thread/list cursors expose each item without changing issue status.
#[tokio::test]
async fn comments_replay_resolve_and_page_by_real_native_url_shape() {
    let mut f = Fixture::new().await;
    let project = f.project().await;
    let issue = f.work("module", &project, None).await;
    let request = json!({"request_id":id(),"target_type":"issue","target_id":issue,"body":"Question for reviewer"});
    f.db.lock().await.lose = Some("MCreateComment".into());
    assert_eq!(
        f.call("add_comment", request.clone()).await.status,
        "outcome_unknown"
    );
    f.restart();
    let root = f.ok("add_comment", request.clone()).await["comment"].clone();
    assert_eq!(
        f.ok("add_comment", request).await["comment"]["id"],
        root["id"]
    );
    let url = root["url"].as_str().unwrap();
    assert!(url.ends_with(&format!("#comment-{}", &root["id"].as_str().unwrap()[..8])));
    assert_eq!(
        f.ok("get_comment", json!({"id":url})).await["comment"]["id"],
        root["id"]
    );

    let reply = f
        .ok(
            "add_comment",
            json!({
                "target_type":"issue","target_id":issue,"parent_id":root["id"],"body":"Answer"
            }),
        )
        .await["comment"]
        .clone();
    assert_eq!(reply["parent"]["id"], root["id"]);
    assert_eq!(
        f.ok("get_comment", json!({"id":root["id"],"first":1}))
            .await["replies"]["nodes"][0]["id"],
        reply["id"]
    );
    let wrong = f.call("add_comment", json!({
        "target_type":"project","target_id":project,"parent_id":root["id"],"body":"Wrong target"
    })).await;
    assert_eq!(wrong.status, "blocked");
    assert_eq!(wrong.data["code"], "INVALID_PARENT");

    let resolved = f
        .ok(
            "resolve_comment",
            json!({
                "id":root["id"],"resolved":true,"resolving_comment_id":reply["id"]
            }),
        )
        .await;
    assert!(resolved["comment"]["resolvedAt"].is_string());
    assert_eq!(
        f.ok("resolve_comment", json!({"id":root["id"],"resolved":true}))
            .await["replayed"],
        true
    );
    assert!(
        f.ok("resolve_comment", json!({"id":root["id"],"resolved":false}))
            .await["comment"]["resolvedAt"]
            .is_null()
    );

    let page1 = f
        .ok(
            "list_items",
            json!({"type":"comment","target_type":"issue","target_id":issue,"first":1}),
        )
        .await;
    assert_eq!(page1["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(page1["pageInfo"]["hasNextPage"], true);
    let page2 = f
        .ok(
            "list_items",
            json!({
                "type":"comment","target_type":"issue","target_id":issue,"first":1,
                "after":page1["pageInfo"]["endCursor"]
            }),
        )
        .await;
    assert_eq!(page2["nodes"].as_array().unwrap().len(), 1);
    assert_ne!(page1["nodes"][0]["id"], page2["nodes"][0]["id"]);
    assert_eq!(
        f.call(
            "get_comment",
            json!({"id":"https://example.com/#comment-12345678"})
        )
        .await
        .data["code"],
        "INVALID_LINK"
    );
}

/// A changed or empty native create payload is uncertain even when target and parent match;
/// identical replay recovers the stored comment, while equivalent list-marker normalization passes.
#[tokio::test]
async fn comment_create_confirms_rendered_body() {
    let mut f = Fixture::new().await;
    let project = f.project().await;
    let issue = f.work("module", &project, None).await;
    for returned_body in ["Changed by provider", ""] {
        let request = json!({"request_id":id(),"target_type":"issue","target_id":issue,
            "body":"Expected content\n\n- one item"});
        f.db.lock().await.comment_response_body = Some(returned_body.into());
        let result = f.call("add_comment", request.clone()).await;
        assert_eq!(result.status, "outcome_unknown");
        assert_eq!(result.data["code"], "NATIVE_STATE_MISMATCH");
        f.restart();
        let replayed = f.ok("add_comment", request).await;
        assert_eq!(replayed["replayed"], true);
        assert!(
            replayed["comment"]["body"]
                .as_str()
                .unwrap()
                .contains("Expected content")
        );
    }
    f.db.lock().await.normalize_lists = true;
    let normalized = f
        .ok(
            "add_comment",
            json!({
                "target_type":"issue","target_id":issue,"body":"Expected content\n\n- one item"
            }),
        )
        .await;
    assert_eq!(normalized["replayed"], false);
    assert!(
        normalized["comment"]["body"]
            .as_str()
            .unwrap()
            .contains("* one item")
    );
}

/// Project and ProjectUpdate comments retain separate native targets and direct links.
#[tokio::test]
async fn comments_cover_project_and_update_targets() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let update_id = id();
    f.db.lock().await.project_updates.insert(update_id.clone(), json!({
        "id":update_id,"url":format!("https://linear.app/project/{project}/updates/{update_id}"),
        "project":{"id":project},"body":"Update","health":"onTrack"
    }));
    for (kind, target) in [
        ("project", project.as_str()),
        ("project_update", update_id.as_str()),
    ] {
        let item = f
            .ok(
                "add_comment",
                json!({
                    "target_type":kind,"target_id":target,"body":"Scoped note"
                }),
            )
            .await["comment"]
            .clone();
        assert_eq!(
            f.ok("get_comment", json!({"id":item["url"]})).await["comment"]["id"],
            item["id"]
        );
        assert_eq!(
            f.ok(
                "list_items",
                json!({
                    "type":"comment","target_type":kind,"target_id":target
                })
            )
            .await["nodes"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}

/// A question requires an addressee and its role/session/source data reaches the normalized reader.
#[tokio::test]
async fn question_activity_is_addressed_and_readable() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let issue = f.work("module", &project, None).await;
    assert_eq!(
        f.call("add_comment",json!({"target_type":"issue","target_id":issue,"kind":"question","body":"Who can review?"})).await.data["code"],
        "INVALID_INPUT"
    );
    let created = f.ok("add_comment",json!({
        "target_type":"issue","target_id":issue,"kind":"question","role":"lead",
        "session":"codex:current","recipient":"reviewer","source_links":["https://example.com/evidence"],
        "body":"Who can review?"
    })).await;
    let record = f
        .ok("get_comment", json!({"id":created["comment"]["id"]}))
        .await["activity"]
        .clone();
    assert_eq!(record["kind"], "question");
    assert_eq!(record["role"], "lead");
    assert_eq!(record["session"], "codex:current");
    assert_eq!(record["recipient"], "reviewer");
    assert_eq!(record["source_links"][0], "https://example.com/evidence");
    assert_eq!(record["body"], "Who can review?");
    assert_eq!(record["formal_review"], false);
}

/// Native ProjectUpdates preserve all health values, native cursors and explicit author/reason
/// while leaving issue status and ordinary project comments separate.
#[tokio::test]
async fn project_updates_create_read_and_page() {
    let mut f = Fixture::new().await;
    let project = f.project().await;
    let issue = f.work("module", &project, None).await;
    let mut updates = vec![];
    for health in ["onTrack", "atRisk", "offTrack"] {
        let args = json!({"request_id":id(),"project_id":project,"health":health,
            "reason":format!("{health} because of observed work"),"body":format!("{health} report")});
        if health == "atRisk" {
            f.db.lock().await.lose = Some("MCreateProjectUpdate".into());
            assert_eq!(
                f.call("save_project_update", args.clone()).await.status,
                "outcome_unknown"
            );
            f.restart();
        }
        let update = f.ok("save_project_update", args.clone()).await;
        assert_eq!(update["project_update"]["health"], health);
        assert_eq!(update["activity"]["health"], health);
        assert_eq!(
            update["activity"]["reason"],
            format!("{health} because of observed work")
        );
        assert_eq!(update["activity"]["actor"], "codex:fixture");
        assert_eq!(
            f.ok("save_project_update", args).await["url"],
            update["url"]
        );
        assert_eq!(
            f.ok(
                "get_context",
                json!({"type":"project_update","id":update["project_update"]["id"]})
            )
            .await["activity"]["id"],
            update["project_update"]["id"]
        );
        updates.push(update["project_update"]["id"].as_str().unwrap().to_owned());
    }
    let mut after = None;
    let mut listed = vec![];
    loop {
        let mut args = json!({"type":"project_update","project_id":project,"first":1});
        if let Some(cursor) = &after {
            args["after"] = json!(cursor);
        }
        let page = f.ok("list_items", args).await;
        assert_eq!(page["nodes"].as_array().unwrap().len(), 1);
        assert_eq!(page["activity_records"].as_array().unwrap().len(), 1);
        listed.push(page["nodes"][0]["id"].as_str().unwrap().to_owned());
        if page["pageInfo"]["hasNextPage"] == false {
            break;
        }
        after = page["pageInfo"]["endCursor"].as_str().map(str::to_owned);
    }
    listed.sort();
    updates.sort();
    assert_eq!(listed, updates);
    f.ok(
        "add_comment",
        json!({"target_type":"project","target_id":project,"body":"Ordinary note"}),
    )
    .await;
    assert_eq!(
        f.ok(
            "list_items",
            json!({"type":"comment","target_type":"project","target_id":project})
        )
        .await["nodes"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        f.ok("get_context", json!({"type":"issue","id":issue}))
            .await["issue"]["state"]["name"],
        "Todo"
    );
}

/// Editing is scoped to the original Project, recovers a lost response from native state, and
/// refuses an intervening native edit when the caller's timestamp is stale.
#[tokio::test]
async fn project_update_edits_require_scope_and_observed_version() {
    let mut f = Fixture::new().await;
    let project = f.project().await;
    let other = f.project().await;
    let created = f.ok("save_project_update",json!({
        "project_id":project,"health":"onTrack","reason":"Work is proceeding","body":"Initial"
    })).await;
    let update_id = created["project_update"]["id"].as_str().unwrap();
    let original_time = created["project_update"]["updatedAt"].as_str().unwrap();
    let wrong = f
        .call(
            "save_project_update",
            json!({
                "id":update_id,"project_id":other,"health":"offTrack",
                "reason":"Wrong project","body":"Wrong","expected_updated_at":original_time
            }),
        )
        .await;
    assert_eq!(wrong.data["code"], "REQUEST_CONFLICT");
    let args = json!({"request_id":id(),"id":update_id,"project_id":project,
        "health":"offTrack","reason":"A blocker appeared","body":"Revised",
        "expected_updated_at":original_time});
    f.db.lock().await.lose = Some("MUpdateProjectUpdate".into());
    assert_eq!(
        f.call("save_project_update", args.clone()).await.status,
        "outcome_unknown"
    );
    f.restart();
    let replayed = f.ok("save_project_update", args.clone()).await;
    assert_eq!(replayed["replayed"], true);
    assert_eq!(replayed["project_update"]["health"], "offTrack");
    assert_eq!(f.db.lock().await.project_updates.len(), 1);
    {
        let mut db = f.db.lock().await;
        db.project_updates.get_mut(update_id).unwrap()["body"] = json!("Manual edit");
        db.project_updates.get_mut(update_id).unwrap()["updatedAt"] = json!("2026-09-26T00:00:00Z");
    }
    let conflict = f.call("save_project_update", args).await;
    assert_eq!(conflict.data["code"], "PENDING_CONFLICT");
    assert_eq!(
        f.db.lock().await.project_updates[update_id]["body"],
        "Manual edit"
    );
}
