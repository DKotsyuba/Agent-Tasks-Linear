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
