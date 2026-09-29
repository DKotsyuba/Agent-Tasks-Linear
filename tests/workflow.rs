//! End-to-end guarded workflow tests using native-shaped Linear HTTP responses.
mod support;
use serde_json::json;
use support::{Fixture, id};

/// Commit imports survive lost replies and unavailable Git, deduplicate linked checkouts, and
/// require explicit current-round results before code closure while retaining native history.
#[tokio::test]
async fn local_commit_imports_are_durable_ordered_and_round_scoped() {
    use std::{fs, path::Path, process::Command};
    /// Execute literal Git fixture setup arguments and return trimmed stdout, requiring success.
    fn git(path: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }
    let root = std::env::temp_dir().join(format!("commit import {}", id()));
    let repo = root.join("repo");
    let linked = root.join("linked");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    let mut hashes = vec![];
    for name in ["first", "second"] {
        let message = format!(
            "feat(import): {name}\n\nResult:\n## Details\n{name} result\n\nChecks:\n{name} passed\nPinned Linear DocumentFilter supports or, issue.id.in, and project.id.eq.\n"
        );
        git(
            &repo,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.test",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                &message,
            ],
        );
        hashes.push(git(&repo, &["rev-parse", "HEAD"]));
    }
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            linked.to_str().unwrap(),
        ],
    );
    let mut f = Fixture::new().await;
    let project = f.project().await;
    let module = f.work("module", &project, None).await;
    f.ok("edit_module", json!({"id":module,"fields":{"repository_path":repo,"repository_url":null,"worktree":repo}})).await;
    f.mv(&module, "In Progress").await;
    let task = f.work("task", &project, Some(&module)).await;
    f.ok(
        "edit_task",
        json!({"id":task,"fields":{"work_type":"code"}}),
    )
    .await;
    f.mv(&task, "In Progress").await;
    let request = json!({"request_id":id(),"work_id":task,"commits":[hashes[0]]});
    f.db.lock().await.lose = Some("MUpdateIssue".into());
    assert_eq!(
        f.call("record_commits", request.clone()).await.status,
        "outcome_unknown"
    );
    {
        let mut db = f.db.lock().await;
        let description = db.issues[&task]["description"].as_str().unwrap();
        assert!(description.contains("issue.id.in"));
        db.issues.get_mut(&task).unwrap()["description"] =
            json!(description.replace("issue.id.in", "[issue.id.in](<http://issue.id.in>)"));
    }
    fs::rename(&repo, root.join("offline")).unwrap();
    f.restart();
    let replayed = f.ok("record_commits", request.clone()).await;
    assert_eq!(replayed["git_reports"].as_array().unwrap().len(), 1);
    assert_eq!(replayed["journal"].as_array().unwrap().len(), 1);
    f.ok("record_commits", request).await;
    let first_activity = f
        .ok(
            "list_items",
            json!({"type":"comment","target_type":"issue","target_id":task}),
        )
        .await;
    assert_eq!(
        first_activity["activity_records"].as_array().unwrap().len(),
        1
    );
    assert_eq!(first_activity["activity_records"][0]["kind"], "progress");
    assert!(
        first_activity["activity_records"][0]["body"]
            .as_str()
            .unwrap()
            .contains("first result")
    );
    assert!(
        first_activity["activity_records"][0]["body"]
            .as_str()
            .unwrap()
            .contains("first passed")
    );
    let snapshot = f.ok("get_context", json!({"id":task,"type":"issue"})).await;
    assert_eq!(snapshot["issue"]["state"]["name"], "In Progress");
    assert!(
        snapshot["git_reports"][0]["original_message"]
            .as_str()
            .unwrap()
            .contains("## Details")
    );
    assert!(
        snapshot["fields"]["result"]
            .as_str()
            .unwrap()
            .contains("### Details")
    );
    assert!(snapshot["discrepancies"].as_array().unwrap().is_empty());
    fs::rename(root.join("offline"), &repo).unwrap();
    f.ok(
        "edit_module",
        json!({"id":module,"fields":{"worktree":linked}}),
    )
    .await;
    let second_request =
        json!({"request_id":id(),"work_id":task,"commits":[hashes[0],hashes[1],hashes[0]]});
    f.db.lock().await.lose = Some("MCreateComment".into());
    assert_eq!(
        f.call("record_commits", second_request.clone())
            .await
            .status,
        "outcome_unknown"
    );
    f.restart();
    let imported = f.ok("record_commits", second_request).await;
    assert_eq!(imported["git_reports"].as_array().unwrap().len(), 2);
    assert_eq!(imported["git_reports"][0]["sha"], hashes[0]);
    assert_eq!(imported["git_reports"][1]["sha"], hashes[1]);
    assert_eq!(imported["journal"].as_array().unwrap().len(), 2);
    let before = f.ok("get_context", json!({"id":task,"type":"issue"})).await;
    f.ok(
        "record_commits",
        json!({"work_id":task,"commits":[hashes[0]]}),
    )
    .await;
    assert_eq!(
        f.ok(
            "list_items",
            json!({"type":"comment","target_type":"issue","target_id":task})
        )
        .await["nodes"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        f.call(
            "record_commits",
            json!({"work_id":task,"commits":[hashes[1],"deadbeefdeadbeef"]})
        )
        .await
        .status,
        "blocked"
    );
    let after = f.ok("get_context", json!({"id":task,"type":"issue"})).await;
    assert_eq!(
        before["workflow"]["revision"],
        after["workflow"]["revision"]
    );
    assert_eq!(before["git_reports"], after["git_reports"]);
    assert!(after["fields"]["commit_url"].is_null());
    f.mv(&task, "Done").await;
    f.mv(&task, "In Progress").await;
    let reopened = f.ok("get_context", json!({"id":task,"type":"issue"})).await;
    assert!(reopened["git_reports"].as_array().unwrap().is_empty());
    assert_eq!(
        reopened["workflow"]["git_reports"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        f.call(
            "move_status",
            json!({"id":task,"status":"Done","actor_role":"worker"})
        )
        .await
        .status,
        "blocked"
    );
    let new_round = f
        .ok(
            "record_commits",
            json!({"work_id":task,"commits":[hashes[0]]}),
        )
        .await;
    assert_eq!(new_round["git_reports"].as_array().unwrap().len(), 1);
    assert_eq!(new_round["git_reports"][0]["round"], 2);
    assert_eq!(
        f.ok(
            "list_items",
            json!({"type":"comment","target_type":"issue","target_id":task})
        )
        .await["nodes"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    f.mv(&task, "Done").await;

    let atomic = f.work("atomic", &project, Some(&module)).await;
    assert_eq!(
        f.call(
            "record_commits",
            json!({"work_id":atomic,"commits":[hashes[0]]})
        )
        .await
        .status,
        "blocked"
    );
    f.ok(
        "edit_atomic",
        json!({"id":atomic,"fields":{"work_type":"code"}}),
    )
    .await;
    f.mv(&atomic, "In Progress").await;
    f.ok(
        "record_commits",
        json!({"work_id":atomic,"commits":[hashes[1]]}),
    )
    .await;
    f.mv(&atomic, "In Review").await;
    f.review(&atomic, "accepted").await;
    f.mv(&atomic, "Done").await;
    f.ok(
        "edit_module",
        json!({"id":module,"fields":{"pr_url":"https://example.test/pull/1"}}),
    )
    .await;
    let ready = f
        .ok("get_context", json!({"id":module,"type":"issue"}))
        .await;
    assert!(ready["fields"]["result"].is_null());
    let guard = f
        .ok(
            "move_status",
            json!({"id":module,"status":"In Review","actor_role":"orchestrator","check_only":true}),
        )
        .await;
    assert_eq!(guard["allowed"], true);
    f.mv(&module, "In Review").await;
    let submitted = f
        .ok("get_context", json!({"id":module,"type":"issue"}))
        .await;
    assert_eq!(
        submitted["fields"]["result"],
        ready["module_report"]["summary"]
    );
    assert_eq!(
        submitted["fields"]["check_result"],
        ready["module_report"]["reported_checks"]
    );
    assert_eq!(submitted["module_report"], ready["module_report"]);
    fs::remove_dir_all(root).unwrap();
    f.restart();
    let cold = f.ok("get_context", json!({"id":task,"type":"issue"})).await;
    assert_eq!(cold["git_reports"][0]["sha"], hashes[0]);
    assert_eq!(cold["workflow"]["git_reports"].as_array().unwrap().len(), 3);
}

/// Local projects preserve prose/documents, inherit real checkouts, and keep PR/merge gates.
/// Uses an isolated real Git repository and linked worktree; fixture writes never contact Linear.
#[tokio::test]
async fn local_repositories_preserve_content_and_support_linked_checkouts() {
    use agent_tasks_linear::records::read_fields;
    use std::{fs, path::Path, process::Command};

    /// Run literal Git arguments in a disposable fixture repository, requiring success.
    /// Returns stdout for before/after comparisons; only setup calls mutate this fixture.
    fn git(path: &Path, args: &[&str]) -> Vec<u8> {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}: {:?}", args, output);
        output.stdout
    }

    let root = std::env::temp_dir().join(format!("local git {}", id()));
    let repo = root.join("repository");
    let linked = root.join("linked checkout");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "Fixture",
        ],
    );
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            linked.to_str().unwrap(),
        ],
    );
    fs::write(repo.join("untracked.txt"), "preserve").unwrap();
    let before = git(&repo, &["status", "--porcelain=v1"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let linked_before = git(&linked, &["status", "--porcelain=v1"]);

    let mut f = Fixture::new().await;
    let request = json!({"request_id":id(),"team_id":f.team,"title":"Local product","description":"Native project description","repository_path":repo});
    let created = f.ok("create_project", request.clone()).await;
    let project = created["project"]["id"].as_str().unwrap().to_owned();
    f.ok("create_project", request).await;
    assert!(
        read_fields(created["project"]["content"].as_str().unwrap()).unwrap()["repository_url"]
            .is_null()
    );
    let prose = "## Репозиторий\n\nLocal sources only; hosting will be decided later.\n\n## Human notes\n\nKeep this paragraph.\n";
    let content = format!("{}{prose}", created["project"]["content"].as_str().unwrap());
    f.db.lock().await.projects.get_mut(&project).unwrap()["content"] = json!(content);
    let documents = f.db.lock().await.documents.clone();
    let unchanged = f
        .ok("edit_project", json!({"id":project,"repository_path":repo}))
        .await;
    assert_eq!(unchanged["content"], content);

    let module = f.work("module", &project, None).await;
    let context = f
        .ok("get_context", json!({"id":module,"type":"issue"}))
        .await;
    assert_eq!(context["fields"]["repository_path"], json!(repo));
    assert!(context["fields"]["repository_url"].is_null());
    let not_ready = f.ok("move_status", json!({"id":module,"status":"In Progress","actor_role":"orchestrator","check_only":true})).await;
    assert_eq!(not_ready["allowed"], false);
    assert!(
        not_ready["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str().unwrap().contains("INVALID_REPOSITORY"))
    );
    f.ok(
        "edit_module",
        json!({"id":module,"fields":{"worktree":linked,"branch":"fixture"}}),
    )
    .await;
    f.mv(&module, "In Progress").await;
    let task = f.work("task", &project, Some(&module)).await;
    f.restart();
    let context = f.ok("get_context", json!({"id":task,"type":"issue"})).await;
    assert_eq!(context["parent_checkout"]["repository_path"], json!(repo));
    assert_eq!(context["parent_checkout"]["worktree"], json!(linked));
    f.mv(&task, "In Progress").await;
    f.result("task", &task).await;
    f.mv(&task, "Done").await;
    f.ok(
        "edit_module",
        json!({"id":module,"fields":{"result":"Implemented","check_result":"Verified"}}),
    )
    .await;
    let review = f
        .ok(
            "move_status",
            json!({"id":module,"status":"In Review","actor_role":"orchestrator","check_only":true}),
        )
        .await;
    assert_eq!(review["conditions"], json!(["Required field: pr_url"]));
    f.result("module", &module).await;
    f.mv(&module, "In Review").await;
    f.review(&module, "accepted").await;
    let done = f
        .ok(
            "move_status",
            json!({"id":module,"status":"Done","actor_role":"orchestrator","check_only":true}),
        )
        .await;
    assert_eq!(done["conditions"], json!(["Required field: merge_report"]));

    let atomic = f.ok("create_atomic", json!({"project_id":project,"team_id":f.team,"title":"Local code","fields":{"work_type":"code","executor":"fixture","expected_result":"Build","acceptance_criteria":"Checks pass","local_check":"Run checks","branch":"fixture","worktree":repo}})).await;
    let atomic_id = atomic["issue"]["id"].as_str().unwrap();
    f.mv(atomic_id, "In Progress").await;
    let context = f
        .ok("get_context", json!({"id":atomic_id,"type":"issue"}))
        .await;
    assert_eq!(context["fields"]["repository_path"], json!(repo));

    f.ok(
        "edit_project",
        json!({"id":project,"repository_url":"https://git.example.test/product"}),
    )
    .await;
    let removed = f
        .ok("edit_project", json!({"id":project,"repository_url":null}))
        .await;
    let fields = read_fields(removed["content"].as_str().unwrap()).unwrap();
    assert_eq!(fields["repository_path"], json!(repo));
    assert_eq!(fields["description"], "Native project description");
    assert!(fields["repository_url"].is_null());
    assert!(
        removed["content"]
            .as_str()
            .unwrap()
            .contains("## Human notes\n\nKeep this paragraph.")
    );
    assert_eq!(f.db.lock().await.documents, documents);
    assert_eq!(f.db.lock().await.projects.len(), 1);
    let cleared = f
        .ok("edit_project", json!({"id":project,"repository_path":null}))
        .await;
    assert!(
        read_fields(cleared["content"].as_str().unwrap()).unwrap()["repository_path"].is_null()
    );
    let existing = f
        .ok("get_context", json!({"id":module,"type":"issue"}))
        .await;
    assert_eq!(existing["fields"]["repository_path"], json!(repo));
    assert_eq!(git(&repo, &["status", "--porcelain=v1"]), before);
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&linked, &["status", "--porcelain=v1"]), linked_before);
    fs::remove_dir_all(root).unwrap();
}

/// Bad paths fail before writes; planning and non-code work need no repository, and legacy URLs work.
#[tokio::test]
async fn repository_validation_preserves_planning_and_legacy_projects() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let original = f.db.lock().await.projects[&project].clone();
    for path in [
        "relative/repository".to_owned(),
        std::env::temp_dir().join(id()).to_str().unwrap().to_owned(),
        std::env::temp_dir().to_str().unwrap().to_owned(),
    ] {
        let rejected = f
            .call("edit_project", json!({"id":project,"repository_path":path}))
            .await;
        assert_eq!(rejected.status, "blocked");
        assert_eq!(rejected.data["code"], "INVALID_REPOSITORY");
        assert_eq!(f.db.lock().await.projects[&project], original);
        assert_eq!(f.call("create_project", json!({"team_id":f.team,"title":"Invalid","description":"No writes","repository_path":path})).await.status, "blocked");
    }
    let planning = f
        .ok(
            "create_project",
            json!({"team_id":f.team,"title":"Planning","description":"No repository yet"}),
        )
        .await;
    let planning_id = planning["project"]["id"].as_str().unwrap();
    let planned_module = f.work("module", planning_id, None).await;
    let readiness = f.ok("move_status", json!({"id":planned_module,"status":"In Progress","actor_role":"orchestrator","check_only":true})).await;
    assert_eq!(
        readiness["conditions"],
        json!(["Required field: repository_path or repository_url"])
    );
    let non_code = f.work("atomic", planning_id, None).await;
    f.mv(&non_code, "In Progress").await;

    let module = f.work("module", &project, None).await;
    assert_eq!(
        f.call(
            "edit_module",
            json!({"id":module,"fields":{"repository_path":"relative"}})
        )
        .await
        .status,
        "blocked"
    );
    f.mv(&module, "In Progress").await;
    let context = f
        .ok("get_context", json!({"id":module,"type":"issue"}))
        .await;
    assert_eq!(
        context["fields"]["repository_url"],
        "https://github.com/example/product"
    );
    assert!(context["fields"]["repository_path"].is_null());
    assert_eq!(f.db.lock().await.projects.len(), 2);
}

/// Run literal Git arguments in a disposable fixture repository, requiring success.
fn git_at(path: &std::path::Path, args: &[&str]) -> Vec<u8> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}: {:?}", args, output);
    output.stdout
}
/// Create one disposable Git repository with an initial commit, returning its absolute path.
fn init_repo(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("{name} {}", id()));
    std::fs::create_dir_all(&path).unwrap();
    git_at(&path, &["init", "-q"]);
    git_at(
        &path,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "Fixture",
        ],
    );
    path
}

/// list_items(type: team) discovers native teams without a Project/Issue filter.
#[tokio::test]
async fn list_items_discovers_teams() {
    let f = Fixture::new().await;
    let teams = f.ok("list_items", json!({"type":"team"})).await;
    let nodes = teams["nodes"].as_array().unwrap();
    assert!(
        nodes
            .iter()
            .any(|t| t["id"] == f.team && t["name"] == "Fixture")
    );
    let scoped = f
        .call("list_items", json!({"type":"team","team_id":f.team}))
        .await;
    assert_eq!(scoped.status, "blocked");
    assert_eq!(scoped.data["code"], "INVALID_INPUT");
}

/// list_items(type: project, repository_path) finds Projects by canonical common Git directory
/// identity: the primary checkout and any linked worktree resolve to the same match, an unknown
/// or non-Git path fails explicitly, several Projects sharing one checkout are all returned, and
/// a Project whose stored path is no longer accessible is reported separately, not silently
/// dropped or fatal to the rest of the lookup.
#[tokio::test]
async fn list_items_finds_projects_by_repository_identity() {
    let repo = init_repo("entry repository");
    let linked = std::env::temp_dir().join(format!("entry linked {}", id()));
    git_at(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            linked.to_str().unwrap(),
        ],
    );
    let stale = init_repo("entry stale");

    let f = Fixture::new().await;
    let primary = f
        .ok(
            "create_project",
            json!({"team_id":f.team,"title":"Primary","description":"Repo one","repository_path":repo}),
        )
        .await["project"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let sibling = f
        .ok(
            "create_project",
            json!({"team_id":f.team,"title":"Sibling","description":"Repo one again","repository_path":repo}),
        )
        .await["project"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let broken = f
        .ok(
            "create_project",
            json!({"team_id":f.team,"title":"Broken","description":"Repo goes away","repository_path":stale}),
        )
        .await["project"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    std::fs::remove_dir_all(&stale).unwrap();

    let by_primary = f
        .ok(
            "list_items",
            json!({"type":"project","repository_path":repo}),
        )
        .await;
    let ids: Vec<_> = by_primary["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap().to_owned())
        .collect();
    assert!(ids.contains(&primary) && ids.contains(&sibling) && !ids.contains(&broken));
    let inaccessible = by_primary["inaccessible_stored_checkouts"]
        .as_array()
        .unwrap();
    assert!(inaccessible.iter().any(|v| v["id"] == broken));

    let by_linked = f
        .ok(
            "list_items",
            json!({"type":"project","repository_path":linked}),
        )
        .await;
    assert_eq!(by_linked["nodes"], by_primary["nodes"]);

    let unknown = f
        .call(
            "list_items",
            json!({"type":"project","repository_path":std::env::temp_dir()}),
        )
        .await;
    assert_eq!(unknown.status, "blocked");
    assert_eq!(unknown.data["code"], "INVALID_REPOSITORY");

    let combined = f
        .call(
            "list_items",
            json!({"type":"project","repository_path":repo,"team_id":f.team}),
        )
        .await;
    assert_eq!(combined.status, "blocked");
    assert_eq!(combined.data["code"], "INVALID_INPUT");

    std::fs::remove_dir_all(&linked).unwrap();
    std::fs::remove_dir_all(&repo).unwrap();
}

/// A brief Project view adds the parsed repository path/url, native teams and an overview route.
#[tokio::test]
async fn project_brief_context_adds_repository_teams_and_overview_route() {
    let repo = init_repo("brief repository");
    let f = Fixture::new().await;
    let created = f
        .ok(
            "create_project",
            json!({"team_id":f.team,"title":"Briefed","description":"Entry passport","repository_path":repo}),
        )
        .await;
    let project = created["project"]["id"].as_str().unwrap();
    let brief = f
        .ok(
            "get_context",
            json!({"type":"project","id":project,"detail":"brief"}),
        )
        .await;
    assert_eq!(brief["project"]["repository_path"], json!(repo));
    assert!(brief["project"]["repository_url"].is_null());
    assert_eq!(
        brief["project"]["teams"].as_array().unwrap()[0]["id"],
        json!(f.team)
    );
    assert!(
        brief["full_context"]["overview"]
            .as_str()
            .unwrap()
            .contains(&format!("get_overview project_id={project}"))
    );
    std::fs::remove_dir_all(repo).unwrap();
}

/// edit_project(content) replaces the whole passport body under an expected_updated_at
/// precondition: it rejects mixing with targeted fields, requires the precondition, confirms a
/// byte-identical retry as replayed without writing, and refuses a stale precondition.
#[tokio::test]
async fn edit_project_content_replace_is_guarded_and_replay_safe() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let created = f.db.lock().await.projects[&project].clone();
    let updated_at = created["updatedAt"].as_str().unwrap().to_owned();

    let mixed = f
        .call(
            "edit_project",
            json!({"id":project,"content":"# New passport\n","title":"Renamed","expected_updated_at":updated_at}),
        )
        .await;
    assert_eq!(mixed.status, "blocked");
    assert_eq!(mixed.data["code"], "INVALID_INPUT");

    let missing_precondition = f
        .call(
            "edit_project",
            json!({"id":project,"content":"# New passport\n"}),
        )
        .await;
    assert_eq!(missing_precondition.status, "blocked");
    assert_eq!(missing_precondition.data["code"], "PRECONDITION_REQUIRED");

    let body = "# New passport\n\nCurated by hand.\n";
    let written = f
        .ok(
            "edit_project",
            json!({"id":project,"content":body,"expected_updated_at":updated_at}),
        )
        .await;
    assert_eq!(written["content"], body);
    assert_eq!(written["replayed"], false);
    let new_updated_at = written["updatedAt"].as_str().unwrap().to_owned();
    assert_ne!(new_updated_at, updated_at);

    let replay = f
        .ok(
            "edit_project",
            json!({"id":project,"content":body,"expected_updated_at":new_updated_at}),
        )
        .await;
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["updatedAt"], json!(new_updated_at));

    let stale = f
        .call(
            "edit_project",
            json!({"id":project,"content":"# Different\n","expected_updated_at":updated_at}),
        )
        .await;
    assert_eq!(stale.status, "blocked");
    assert_eq!(stale.data["code"], "PENDING_CONFLICT");
}

/// Linear may change list markers, but code, literal markers, words and destinations remain significant.
#[test]
fn markdown_list_markers_preserve_content() {
    use agent_tasks_linear::records::markdown_key;
    let original =
        "## План\n- [Результат](https://linear.app/example/issue/TEST-1/result): готово.";
    let native =
        "## План\n\n* [Результат](<https://linear.app/example/issue/TEST-1/result>): готово.";
    assert_eq!(markdown_key(original), markdown_key(native));
    for (before, after) in [
        ("- `inline`\n- next", "* `inline`\n* next"),
        ("- ```text\n  - item\n  ```", "* ```text\n  - item\n  ```"),
    ] {
        assert_eq!(markdown_key(before), markdown_key(after));
    }
    assert_ne!(
        markdown_key(original),
        markdown_key(&native.replace("готово", "отложено"))
    );
    assert_ne!(
        markdown_key(original),
        markdown_key(&native.replace("TEST-1", "TEST-2"))
    );
    for (before, after) in [
        ("```text\n- item\n```", "```text\n* item\n```"),
        ("~~~\n- item\n~~~", "~~~\n* item\n~~~"),
        ("    - item", "    * item"),
        ("\\- item", "- item"),
        ("- item", "\\* item"),
        ("text `first\n- item\nlast`", "text `first\n* item\nlast`"),
        ("- ```text\n  - item\n  ```", "- ```text\n  * item\n  ```"),
        ("- - -", "* - -"),
    ] {
        assert_ne!(markdown_key(before), markdown_key(after), "{before}");
    }
}

/// A native same-label HTTP link may represent a bare domain in prose, but altered links cannot.
#[test]
fn markdown_bare_domain_autolink_preserves_meaning() {
    use agent_tasks_linear::records::markdown_equivalent;
    let expected =
        "## Checks\n\n- Pinned Linear DocumentFilter supports or, issue.id.in, and project.id.eq.";
    let native = "## Checks\n\n* Pinned Linear DocumentFilter supports or, [issue.id.in](<http://issue.id.in>), and project.id.eq.";
    assert!(markdown_equivalent(expected, native));
    for changed in [
        native.replace("http://issue.id.in", "http://different.id.in"),
        native.replace("[issue.id.in]", "[different label]"),
        native.replace(", and", ", unexpectedly and"),
        native.replace("[issue.id.in]", "[issue.id.out]"),
    ] {
        assert!(!markdown_equivalent(expected, &changed), "{changed}");
    }
    assert!(!markdown_equivalent(
        "`issue.id.in`",
        "[issue.id.in](<http://issue.id.in>)"
    ));
    assert!(!markdown_equivalent(
        "prefixissue.id.in",
        "prefix[issue.id.in](<http://issue.id.in>)"
    ));
    assert!(!markdown_equivalent(
        "issue.id.in.foo",
        "[issue.id.in](<http://issue.id.in>).foo"
    ));
}

/// A legacy pending edit finalizes after native list serialization while manual content edits still block.
#[tokio::test]
async fn markdown_list_pending_retry_preserves_manual_changes() {
    let mut f = Fixture::new().await;
    let project = f.project().await;
    let epic = f.work("epic", &project, None).await;
    let request = json!({"request_id":id(),"id":epic,"fields":{"description":"### План\n\n- [Результат](https://linear.app/example/issue/TEST-1/result): готово."}});
    {
        let mut db = f.db.lock().await;
        db.normalize_lists = true;
        db.lose = Some("MUpdateIssue".into());
    }
    assert_eq!(
        f.call("edit_epic", request.clone()).await.status,
        "outcome_unknown"
    );
    let native = f.db.lock().await.issues[&epic]["description"]
        .as_str()
        .unwrap()
        .replace("](https://", "](<https://")
        .replace("/result)", "/result>)");
    for manual in [
        native.replace("готово", "отложено"),
        native.replace("TEST-1", "TEST-2"),
        native.replace("* [", "\\* ["),
    ] {
        f.db.lock().await.issues.get_mut(&epic).unwrap()["description"] = json!(manual);
        f.restart();
        let conflict = f.call("edit_epic", request.clone()).await;
        assert_eq!(conflict.data["code"], "PENDING_CONFLICT");
        assert_eq!(f.db.lock().await.issues[&epic]["description"], manual);
    }
    f.db.lock().await.issues.get_mut(&epic).unwrap()["description"] = json!(native);
    f.restart();
    f.ok("edit_epic", request.clone()).await;
    f.ok("edit_epic", request).await;
    let context = f.ok("get_context", json!({"type":"issue","id":epic})).await;
    assert!(context["workflow"]["pending"].is_null());
    assert_eq!(context["discrepancies"], json!([]));
    assert_eq!(context["issue"]["description"], native);
}

/// Native titles replace only requested bare URLs; explicit labels, destinations, words,
/// code literals and extra sections remain substantive differences.
#[test]
fn markdown_native_link_title_preserves_meaningful_differences() {
    use agent_tasks_linear::records::markdown_equivalent;
    let url = "https://linear.app/example/document/spec-123";
    let expected = format!("Plan: {url}\n\nKeep [Role](https://example.com/role)");
    let native = expected.replace(url, &format!("[Spec title](<{url}>)"));
    assert!(markdown_equivalent(&expected, &native));
    assert!(markdown_equivalent(
        &expected,
        &native.replace("Spec title", "New spec title")
    ));
    for changed in [
        native.replace("[Spec title]", "[Extra] [Spec title]"),
        native.replace("spec-123", "spec-124"),
        native.replace("Plan:", "Changed:"),
        native.replace("[Role]", "[Other]"),
        format!("{native}\n\n## Notes\nExtra text"),
    ] {
        assert!(!markdown_equivalent(&expected, &changed), "{changed}");
    }
    assert!(!markdown_equivalent(
        &format!("[Original](<{url}>)"),
        &format!("[Changed](<{url}>)"),
    ));
    assert!(!markdown_equivalent(
        &format!("`{url}`"),
        &format!("`[Spec title](<{url}>)`"),
    ));
    // A sentence period, comma or closing bracket directly after the bare URL is where
    // Linear closes its generated title link; the URL itself is unchanged.
    let sentence = format!("Plan {url}. Consume existing semantics at the pinned base.");
    let native_sentence = sentence.replace(url, &format!("[Spec title](<{url}>)"));
    assert!(markdown_equivalent(&sentence, &native_sentence));
    assert!(markdown_equivalent(
        &format!("See {url}, then continue."),
        &format!("See [Spec title](<{url}>), then continue.")
    ));
    assert!(markdown_equivalent(
        &format!("(see {url}) done."),
        &format!("(see [Spec title](<{url}>)) done.")
    ));
    for changed in [
        native_sentence.replace("spec-123", "spec-124"),
        native_sentence.replace("Consume existing", "Adopt and consume existing"),
    ] {
        assert!(!markdown_equivalent(&sentence, &changed), "{changed}");
    }
    // A period followed by alphanumeric continues the requested URL, so it is not a boundary.
    assert!(!markdown_equivalent(
        &format!("See {url}.foo next."),
        &format!("See [Spec title](<{url}>).foo next.")
    ));
}

/// A pending Issue edit accepts Linear's title for an originally bare document URL without
/// changing the requested words, other link labels, or its saved request identity.
#[tokio::test]
async fn markdown_document_title_pending_retry_preserves_content() {
    let mut f = Fixture::new().await;
    let project = f.project().await;
    let module = f.work("module", &project, None).await;
    let url = "https://linear.app/example/document/spec-123";
    let request = json!({"request_id":id(),"id":module,"fields":{
        "description":format!("Plan: {url}\n\nKeep [Role](https://example.com/role)")
    }});
    f.db.lock().await.lose = Some("MUpdateIssue".into());
    assert_eq!(
        f.call("edit_module", request.clone()).await.status,
        "outcome_unknown"
    );
    let native = f.db.lock().await.issues[&module]["description"]
        .as_str()
        .unwrap()
        .replace(url, &format!("[Spec title](<{url}>)"));
    f.db.lock().await.issues.get_mut(&module).unwrap()["description"] = json!(native);
    f.restart();
    f.ok("edit_module", request.clone()).await;
    f.ok("edit_module", request).await;
    let context = f
        .ok("get_context", json!({"type":"issue","id":module}))
        .await;
    assert!(context["workflow"]["pending"].is_null());
    assert_eq!(context["discrepancies"], json!([]));
    assert_eq!(context["issue"]["description"], native);
}

/// A native same-label mailto link may represent a requested email autolink in prose, but
/// altered destinations, labels, extra prose and code literals cannot.
#[test]
fn markdown_mailto_autolink_preserves_meaning() {
    use agent_tasks_linear::records::markdown_equivalent;
    let email = "noreply@anthropic.com";
    let expected = format!(
        "Checks:\ncargo test --workspace --locked\n\nCo-Authored-By: Claude Code <{email}>"
    );
    let native = expected.replace(
        &format!("<{email}>"),
        &format!("[{email}](<mailto:{email}>)"),
    );
    assert!(markdown_equivalent(&expected, &native));
    for changed in [
        native.replace("mailto:noreply", "mailto:different"),
        native.replace(&format!("[{email}]"), "[different@example.test]"),
        native.replace("Co-Authored-By", "Also reviewed by"),
        format!("{expected} extra"),
    ] {
        assert!(!markdown_equivalent(&expected, &changed), "{changed}");
    }
    assert!(!markdown_equivalent(
        &format!("`<{email}>`"),
        &format!("`[{email}](<mailto:{email}>)`"),
    ));
    assert!(!markdown_equivalent(
        &format!("see <{email}> now"),
        &format!("see [{email}](<mailto:different@example.test>) now"),
    ));
    assert!(!markdown_equivalent(
        "see <a@localhost> now",
        "see [a@localhost](<mailto:a@localhost>) now",
    ));

    // A bare email address in prose becomes the same native mailto link as the
    // angle-bracketed autolink; altered destinations, labels and prose still differ.
    let bare = format!("Contact {email}; end.");
    let bare_native = bare.replace(email, &format!("[{email}](<mailto:{email}>)"));
    assert!(markdown_equivalent(&bare, &bare_native));
    for changed in [
        bare_native.replace("mailto:noreply", "mailto:different"),
        bare_native.replace(&format!("[{email}]"), "[different@example.test]"),
        format!("{bare} extra"),
    ] {
        assert!(!markdown_equivalent(&bare, &changed), "{changed}");
    }
    assert!(!markdown_equivalent(
        &format!("see {email} now"),
        &format!("see [{email}](<mailto:different@example.test>) now"),
    ));

    // The exact live combination: bare email, bare domain and a bare document URL that
    // Linear serialized in one stored body all compare as the same meaning.
    let url =
        "https://linear.app/example/document/proverka-sohrannosti-teksta-dokumenta-9f462721f57d";
    let segment = format!("Control: {email}; docs.rs; {url} . Retry.");
    let native_segment = segment
        .replace(email, &format!("[{email}](<mailto:{email}>)"))
        .replace("docs.rs", "[docs.rs](<http://docs.rs>)")
        .replace(url, &format!("[Проверка сохранности текста](<{url}>)"));
    assert!(markdown_equivalent(&segment, &native_segment));
    assert!(!markdown_equivalent(
        &segment,
        &native_segment.replace("mailto:noreply", "mailto:different"),
    ));
}

/// A pending commit import accepts Linear's mailto autolink for a co-author footer while the
/// exact retry finalizes without conflicts or drift.
#[tokio::test]
async fn markdown_mailto_pending_retry_preserves_content() {
    use std::{fs, path::Path, process::Command};
    /// Execute literal Git fixture setup arguments and return trimmed stdout, requiring success.
    fn git(path: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }
    let root = std::env::temp_dir().join(format!("commit mailto {}", id()));
    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    let message = "feat(catalog): accept typed permalinks\n\nResult:\nAdded reference inputs.\n\nChecks:\ncargo test --workspace --locked\n\nCo-Authored-By: Claude Code <noreply@anthropic.com>\n";
    git(
        &repo,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            message,
        ],
    );
    let hash = git(&repo, &["rev-parse", "HEAD"]);
    let mut f = Fixture::new().await;
    let project = f.project().await;
    let module = f.work("module", &project, None).await;
    f.ok("edit_module", json!({"id":module,"fields":{"repository_path":repo,"repository_url":null,"worktree":repo}})).await;
    f.mv(&module, "In Progress").await;
    let task = f.work("task", &project, Some(&module)).await;
    f.ok(
        "edit_task",
        json!({"id":task,"fields":{"work_type":"code"}}),
    )
    .await;
    f.mv(&task, "In Progress").await;
    let request = json!({"request_id":id(),"work_id":task,"commits":[hash]});
    f.db.lock().await.lose = Some("MUpdateIssue".into());
    assert_eq!(
        f.call("record_commits", request.clone()).await.status,
        "outcome_unknown"
    );
    let email = "noreply@anthropic.com";
    let native = f.db.lock().await.issues[&task]["description"]
        .as_str()
        .unwrap()
        .replace(
            &format!("<{email}>"),
            &format!("[{email}](<mailto:{email}>)"),
        );
    f.db.lock().await.issues.get_mut(&task).unwrap()["description"] = json!(native.clone());
    f.restart();
    f.ok("record_commits", request.clone()).await;
    f.ok("record_commits", request).await;
    let context = f.ok("get_context", json!({"id":task,"type":"issue"})).await;
    assert!(context["workflow"]["pending"].is_null());
    assert_eq!(context["discrepancies"], json!([]));
    assert_eq!(context["issue"]["description"], native);
    fs::remove_dir_all(root).unwrap();
}

/// A pending Issue edit accepts Linear's titled link when the requested bare document URL is
/// followed directly by sentence punctuation, and a same-sentence domain autolink; the exact
/// retry finalizes without conflicts or drift.
#[tokio::test]
async fn markdown_punctuation_url_pending_retry_preserves_content() {
    let mut f = Fixture::new().await;
    let project = f.project().await;
    let module = f.work("module", &project, None).await;
    let url = "https://linear.app/example/document/spec-123";
    let request = json!({"request_id":id(),"id":module,"fields":{
        "required_contract":format!(
            "Plan {url}. Coordinate lib.rs registration and [Role](https://example.com/role)."
        )
    }});
    f.db.lock().await.lose = Some("MUpdateIssue".into());
    assert_eq!(
        f.call("edit_module", request.clone()).await.status,
        "outcome_unknown"
    );
    let native = f.db.lock().await.issues[&module]["description"]
        .as_str()
        .unwrap()
        .to_owned()
        .replace(url, &format!("[Spec title](<{url}>)"))
        .replace(
            "Coordinate lib.rs registration",
            "Coordinate [lib.rs](<http://lib.rs>) registration",
        );
    f.db.lock().await.issues.get_mut(&module).unwrap()["description"] = json!(native.clone());
    f.restart();
    f.ok("edit_module", request.clone()).await;
    f.ok("edit_module", request).await;
    let context = f
        .ok("get_context", json!({"type":"issue","id":module}))
        .await;
    assert!(context["workflow"]["pending"].is_null());
    assert_eq!(context["discrepancies"], json!([]));
    assert_eq!(context["issue"]["description"], native);
}

/// Native same-meaning URL serialization replays safely after a lost write across comments,
/// reviews, documents, partial work creation and ProjectUpdates, while changed destinations
/// still conflict. Every retry reuses the exact original arguments; only Linear's own
/// serialization of the stored body differs.
#[tokio::test]
async fn native_serialization_replays_exact_operations() {
    let mut f = Fixture::new().await;
    let project = f.project().await;
    let module = f.work("module", &project, None).await;
    f.mv(&module, "In Progress").await;
    let url = "https://linear.app/example/document/spec-123";
    let titled = format!("[Spec title](<{url}>)");

    // Comment replay: the stored body gained a native title for the bare URL.
    let comment_request = json!({"request_id":id(),"target_type":"issue","target_id":module,
        "kind":"progress","body":format!("Read {url}. Then continue.")});
    let comment = f.ok("add_comment", comment_request.clone()).await["comment"].clone();
    {
        let mut db = f.db.lock().await;
        let stored = db
            .comments
            .get_mut(comment["id"].as_str().unwrap())
            .unwrap();
        stored["body"] = json!(stored["body"].as_str().unwrap().replace(url, &titled));
    }
    let replayed = f.ok("add_comment", comment_request.clone()).await;
    assert_eq!(replayed["replayed"], json!(true));
    assert_eq!(replayed["comment"]["id"], comment["id"]);
    // A changed destination is a real conflict, not a serialization difference.
    {
        let mut db = f.db.lock().await;
        let stored = db
            .comments
            .get_mut(comment["id"].as_str().unwrap())
            .unwrap();
        stored["body"] = json!(stored["body"].as_str().unwrap().replace(
            &titled,
            "[Spec title](<https://linear.app/example/document/spec-124>)",
        ));
    }
    assert_eq!(
        f.call("add_comment", comment_request).await.data["code"],
        "REQUEST_CONFLICT"
    );

    // Comment post-create confirmation: the creation response itself carries the native
    // serialization, which must not read as an unconfirmed write.
    let confirm_request = json!({"request_id":id(),"target_type":"issue","target_id":module,
        "kind":"note","body":format!("See {url}.")});
    {
        let mut db = f.db.lock().await;
        db.comment_response_body = Some(format!(
            "Activity: v1\nKind: note\nRole: participant\nActor: codex:fixture\n\nSee {titled}."
        ));
    }
    let confirmed = f.ok("add_comment", confirm_request).await;
    let stored_body =
        f.db.lock().await.comments[confirmed["comment"]["id"].as_str().unwrap()]["body"]
            .as_str()
            .unwrap()
            .to_owned();
    assert_eq!(
        stored_body,
        format!("Activity: v1\nKind: note\nRole: participant\nActor: codex:fixture\n\nSee {url}.")
    );

    // Review replay: the report comment exists while its workflow record was never saved
    // (a crash between comment creation and metadata persistence).
    f.result("module", &module).await;
    f.mv(&module, "In Review").await;
    let review_request = json!({"request_id":id(),"id":module,"reviewer":"codex:reviewer",
        "verdict":"accepted","summary":format!("Checked {url}."),"findings":"",
        "artifacts":["https://example.com/report"]});
    let review = f.ok("record_review", review_request.clone()).await;
    {
        let mut db = f.db.lock().await;
        let stored = db
            .comments
            .get_mut(review["comment"]["id"].as_str().unwrap())
            .unwrap();
        stored["body"] = json!(stored["body"].as_str().unwrap().replace(url, &titled));
        let attachment = db
            .attachments
            .values_mut()
            .find(|a| a["issue"]["id"] == json!(module))
            .unwrap();
        attachment["metadata"]["workflow"]["last_request"] = serde_json::Value::Null;
    }
    let reviewed = f.ok("record_review", review_request).await;
    assert!(
        reviewed["comment"]["body"]
            .as_str()
            .unwrap()
            .contains(&titled)
    );

    // Document creation retry: the stored content was serialized natively.
    let document_request = json!({"request_id":id(),"issue_id":module,
        "title":"Serialization guide","content":format!("See {url}.")});
    let document = f.ok("save_document", document_request.clone()).await;
    {
        let mut db = f.db.lock().await;
        let stored = db
            .documents
            .get_mut(document["id"].as_str().unwrap())
            .unwrap();
        stored["content"] = json!(format!("See {titled}."));
    }
    f.restart();
    let document_replay = f.ok("save_document", document_request).await;
    assert_eq!(document_replay["id"], document["id"]);

    // Partially created work: the Issue exists without its metadata attachment.
    let work_id = id();
    let create_request = json!({"request_id":work_id,"team_id":f.team,"project_id":project,
        "parent_id":module,"title":"Serialization task","fields":{"work_type":"non_code",
        "description":format!("Plan {url}.")}});
    f.db.lock().await.lose = Some("MCreateIssue".into());
    assert_eq!(
        f.call("create_task", create_request.clone()).await.status,
        "outcome_unknown"
    );
    {
        let mut db = f.db.lock().await;
        let stored = db.issues.get_mut(&work_id).unwrap();
        stored["description"] = json!(format!(
            "## Описание\n\nPlan {titled}.\n\n## Вид работы\n\nnon_code\n\n"
        ));
    }
    f.restart();
    let created = f.ok("create_task", create_request).await;
    assert_eq!(created["issue"]["id"], json!(work_id));

    // ProjectUpdate creation retry after a lost response, and a natively serialized
    // creation response that must still confirm.
    let update_id = id();
    let update_request = json!({"request_id":update_id,"project_id":project,
        "health":"atRisk","reason":"Verifying","body":format!("Overview {url}.")});
    f.db.lock().await.lose = Some("MCreateProjectUpdate".into());
    assert_eq!(
        f.call("save_project_update", update_request.clone())
            .await
            .status,
        "outcome_unknown"
    );
    {
        let mut db = f.db.lock().await;
        let stored = db.project_updates.get_mut(&update_id).unwrap();
        stored["body"] = json!(format!(
            "Author: codex:fixture\nReason: Verifying\n\nOverview {titled}."
        ));
    }
    f.restart();
    let update_replay = f.ok("save_project_update", update_request).await;
    assert_eq!(update_replay["replayed"], json!(true));
    let confirm_update = json!({"request_id":id(),"project_id":project,
        "health":"onTrack","reason":"Confirmed","body":format!("Second {url}.")});
    f.db.lock().await.update_response_body = Some(format!(
        "Author: codex:fixture\nReason: Confirmed\n\nSecond {titled}."
    ));
    let confirmed_update = f.ok("save_project_update", confirm_update).await;
    assert_eq!(confirmed_update["replayed"], json!(false));
}

/// A document whose stored body carries Linear's autolinks for a bare email address, a bare
/// domain and a bare document URL replays its exact creation request after a cold restart.
#[tokio::test]
async fn bare_email_document_serialization_replays_after_cold_restart() {
    let mut f = Fixture::new().await;
    let project = f.project().await;
    let module = f.work("module", &project, None).await;
    let email = "noreply@anthropic.com";
    let url =
        "https://linear.app/example/document/proverka-sohrannosti-teksta-dokumenta-9f462721f57d";
    let document_request = json!({"request_id":id(),"issue_id":module,
        "title":"Установка и квалификация",
        "content":format!(
            "Контроль native serialization: {email}; docs.rs; {url} . Точный повтор после холодного запуска.")});
    let document = f.ok("save_document", document_request.clone()).await;
    {
        let mut db = f.db.lock().await;
        let stored = db
            .documents
            .get_mut(document["id"].as_str().unwrap())
            .unwrap();
        let native = stored["content"]
            .as_str()
            .unwrap()
            .replace(email, &format!("[{email}](<mailto:{email}>)"))
            .replace("docs.rs", "[docs.rs](<http://docs.rs>)")
            .replace(url, &format!("[Проверка сохранности текста](<{url}>)"));
        stored["content"] = json!(native);
    }
    f.restart();
    let replayed = f.ok("save_document", document_request).await;
    assert_eq!(replayed["id"], document["id"]);
}

/// Typed permalink arguments drive guarded operations through resolved identities while the
/// exact original arguments stay the replay key: a lost edit reply retries without conflict
/// or duplicate, comment replies compare resolved parents, and field references store UUIDs.
#[tokio::test]
async fn permalink_arguments_replay_exact_operations() {
    let mut f = Fixture::new().await;
    let project = f.project().await;
    let epic = f.work("epic", &project, None).await;
    let module = f.work("module", &project, Some(&epic)).await;
    let standalone = f.work("module", &project, None).await;
    let project_slug = "passport-b1b2c3d4e5";
    let (epic_link, module_link, project_link) = {
        let mut db = f.db.lock().await;
        let epic_link = format!(
            "https://linear.app/example/issue/{}/readable-epic",
            db.issues[&epic]["identifier"].as_str().unwrap()
        );
        db.issues.get_mut(&epic).unwrap()["url"] = json!(epic_link);
        let module_link = format!(
            "https://linear.app/example/issue/{}/readable-module",
            db.issues[&module]["identifier"].as_str().unwrap()
        );
        db.issues.get_mut(&module).unwrap()["url"] = json!(module_link);
        let project_link = format!("https://linear.app/example/project/{project_slug}");
        db.projects.get_mut(&project).unwrap()["url"] = json!(project_link);
        (epic_link, module_link, project_link)
    };
    f.mv(&epic, "In Progress").await;
    f.ok(
        "move_status",
        json!({"id":module_link,"status":"In Progress","actor_role":"orchestrator"}),
    )
    .await;

    // Field references resolve to canonical UUIDs before validation and storage.
    f.ok(
        "edit_module",
        json!({"id":standalone,"fields":{"after_epic":epic_link}}),
    )
    .await;
    let atomic = f.work("atomic", &project, Some(&epic)).await;
    f.ok(
        "edit_atomic",
        json!({"id":atomic,"fields":{
            "work_type":"integration",
            "integration_modules":[module_link, epic_link]
        }}),
    )
    .await;
    let stored = f
        .ok("get_context", json!({"type":"issue","id":standalone}))
        .await;
    assert_eq!(stored["fields"]["after_epic"], json!(epic));
    let integration = f
        .ok("get_context", json!({"type":"issue","id":atomic}))
        .await;
    assert_eq!(
        integration["fields"]["integration_modules"],
        json!([module, epic])
    );

    // A permalink parent creates work; the created record stores the resolved parent UUID.
    let task = f
        .ok(
            "create_task",
            json!({"project_id":project_link,"team_id":f.team,"parent_id":module_link,
                "title":"Permalink driven","fields":{"work_type":"non_code"}}),
        )
        .await["issue"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let task_link = {
        let mut db = f.db.lock().await;
        let link = format!(
            "https://linear.app/example/issue/{}/readable-task",
            db.issues[&task]["identifier"].as_str().unwrap()
        );
        db.issues.get_mut(&task).unwrap()["url"] = json!(link);
        link
    };
    let created = f
        .ok("get_context", json!({"type":"issue","id":&task}))
        .await;
    assert_eq!(created["workflow"]["parent_id"], json!(module));

    // A lost edit reply recovers from the exact permalink arguments without a duplicate write.
    let request = json!({"request_id":id(),"id":task_link,"fields":{
        "description":"Continued by permalink","expected_result":"Same identity"
    }});
    f.db.lock().await.lose = Some("MUpdateIssue".into());
    assert_eq!(
        f.call("edit_task", request.clone()).await.status,
        "outcome_unknown"
    );
    f.restart();
    f.ok("edit_task", request.clone()).await;
    let replayed = f.ok("edit_task", request).await;
    assert_eq!(replayed["replayed"], true);
    let recovered = f
        .ok("get_context", json!({"type":"issue","id":&task}))
        .await;
    assert!(recovered["workflow"]["pending"].is_null());
    assert_eq!(recovered["fields"]["description"], "Continued by permalink");

    // Comment targets and reply parents compare resolved identities on create and replay.
    let root = f
        .ok(
            "add_comment",
            json!({"target_type":"issue","target_id":module_link,"body":"Root note"}),
        )
        .await["comment"]
        .clone();
    let reply_request = json!({"request_id":id(),"target_type":"issue","target_id":module_link,
        "parent_id":root["url"],"body":"Reply by permalink"});
    let reply = f.ok("add_comment", reply_request.clone()).await;
    assert_eq!(reply["comment"]["parent"]["id"], root["id"]);
    assert_eq!(
        f.ok("add_comment", reply_request).await["replayed"],
        json!(true)
    );

    // A permalink document parent replays its resolved ownership without a duplicate document.
    let document_request = json!({"request_id":id(),"issue_id":module_link,
        "title":"Permalink guide","content":"Body"});
    let document = f.ok("save_document", document_request.clone()).await;
    assert_eq!(document["issue"]["id"], json!(module));
    assert_eq!(
        f.ok("save_document", document_request).await["id"],
        document["id"]
    );
}

/// check_only previews the exact effects of an allowed transition without writing, and the
/// executing transition clears exactly the previewed fields; a blocked preview carries only
/// conditions and no effect plan.
#[tokio::test]
async fn check_only_preview_matches_executed_effects() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let epic = f.work("epic", &project, None).await;
    let module = f.work("module", &project, Some(&epic)).await;
    f.mv(&epic, "In Progress").await;
    f.mv(&module, "In Progress").await;
    f.result("module", &module).await;
    f.mv(&module, "In Review").await;
    f.review(&module, "accepted").await;
    f.ok(
        "edit_module",
        json!({"id":module,"fields":{"merge_report":"PR merged into main"}}),
    )
    .await;
    f.mv(&module, "Done").await;

    let before = f.db.lock().await.issues[&module].clone();
    let preview = f
        .ok(
            "move_status",
            json!({"id":module,"status":"In Progress","actor_role":"orchestrator","check_only":true}),
        )
        .await;
    assert_eq!(preview["allowed"], true);
    assert_eq!(preview["status"], "In Progress");
    let effects = &preview["effects"];
    let clears: Vec<&str> = effects["clears"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    for field in ["result", "check_result", "merge_report", "pr_url"] {
        assert!(clears.contains(&field), "{clears:?}");
    }
    assert_eq!(effects["round_changes"], true);
    assert_eq!(effects["review_invalidated"], true);
    assert_eq!(f.db.lock().await.issues[&module], before);

    f.mv(&module, "In Progress").await;
    let reopened = f
        .ok("get_context", json!({"type":"issue","id":module}))
        .await;
    for field in ["result", "check_result", "merge_report", "pr_url"] {
        assert!(reopened["fields"][field].is_null(), "{field}");
    }
    assert_eq!(reopened["workflow"]["round"], 2);
    assert!(reopened["workflow"]["review"].is_null());

    let blocked = f
        .ok(
            "move_status",
            json!({"id":module,"status":"Done","actor_role":"worker","check_only":true}),
        )
        .await;
    assert_eq!(blocked["allowed"], false);
    assert!(blocked["effects"].is_null());
    assert!(!blocked["conditions"].as_array().unwrap().is_empty());
    assert_eq!(
        f.db.lock().await.issues[&module]["state"]["name"],
        "In Progress"
    );
}

/// Confirmed mutations carry post-write guidance from the same helper as reads, a lost
/// response keeps its recovery advice with the exact retry tool, and the confirmed status
/// never degrades into a failure.
#[tokio::test]
async fn acknowledgements_carry_next_action_guidance() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let epic = f.work("epic", &project, None).await;
    let module = f.work("module", &project, Some(&epic)).await;
    f.mv(&epic, "In Progress").await;
    f.mv(&module, "In Progress").await;
    f.result("module", &module).await;
    let moved = f
        .ok(
            "move_status",
            json!({"id":module,"status":"In Review","actor_role":"orchestrator"}),
        )
        .await;
    assert_eq!(moved["guidance"]["work_id"], json!(module));
    assert_eq!(moved["guidance"]["stage"], "review");
    assert_eq!(moved["guidance"]["next_action"]["kind"], "record_review");
    assert_eq!(moved["guidance"]["next_action"]["tool"], "record_review");
    assert_eq!(moved["guidance"]["next_action"]["actor_role"], "reviewer");

    let edited = f
        .ok(
            "edit_module",
            json!({"id":module,"title":"Refined direction"}),
        )
        .await;
    assert_eq!(edited["guidance"]["stage"], "review");

    f.db.lock().await.lose = Some("MUpdateIssue".into());
    let request = json!({"request_id":id(),"id":module,"title":"Still reviewing"});
    assert_eq!(
        f.call("edit_module", request.clone()).await.status,
        "outcome_unknown"
    );
    let context = f
        .ok(
            "get_context",
            json!({"type":"issue","id":module,"view":"lead"}),
        )
        .await;
    assert_eq!(context["guidance"]["stage"], "recovery");
    assert_eq!(
        context["guidance"]["next_action"]["kind"],
        "retry_operation"
    );
    assert_eq!(context["guidance"]["next_action"]["tool"], "edit_module");
    let brief = f
        .ok(
            "get_context",
            json!({"type":"issue","id":module,"detail":"brief"}),
        )
        .await;
    assert_eq!(brief["guidance"]["stage"], "recovery");
}

/// A freshly created, fully prepared work item reports truthful preparation guidance instead
/// of a spurious ancestor-graph drift caused by its own absence from the pre-write graph
/// snapshot; its guidance matches a fresh context read exactly. An incomplete real parent
/// (still Backlog) is still diagnosed, never silently treated as ready.
#[tokio::test]
async fn create_acknowledges_healthy_work_with_truthful_guidance() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let epic = f.work("epic", &project, None).await;

    // A real incomplete parent (Epic still Backlog) is diagnosed, not silently accepted.
    let blocked = f
        .ok(
            "create_module",
            json!({"team_id":f.team,"project_id":project,"parent_id":epic,"title":"Readable module","fields":{
                "description":"Human readable work","expected_result":"Observable result",
                "acceptance_criteria":"Scenarios pass","lead":"codex:lead","branch":"feature/example",
                "worktree":"/tmp/example","required_contract":"Not required","provided_contract":"Documented API"
            }}),
        )
        .await;
    let module = blocked["issue"]["id"].as_str().unwrap().to_owned();
    assert_eq!(blocked["guidance"]["stage"], "preparation");
    assert_ne!(
        blocked["guidance"]["conditions"],
        json!(["Ancestor is outside the project graph"]),
        "a healthy new item must not report a spurious ancestor-graph drift"
    );

    f.mv(&epic, "In Progress").await;
    let created = f
        .ok(
            "create_task",
            json!({"team_id":f.team,"project_id":project,"parent_id":module,"title":"Readable task","fields":{
                "description":"Human readable work","expected_result":"Observable result",
                "acceptance_criteria":"Scenarios pass","work_type":"non_code",
                "local_check":"Inspect output","executor":"codex:worker"
            }}),
        )
        .await;
    assert_eq!(created["guidance"]["stage"], "preparation");
    assert_eq!(created["guidance"]["conditions"], json!([]));
    assert_eq!(created["guidance"]["next_action"]["kind"], "prepare_work");

    let task = created["issue"]["id"].as_str().unwrap().to_owned();
    let fresh = f
        .ok(
            "get_context",
            json!({"type":"issue","id":task,"view":"lead"}),
        )
        .await;
    assert_eq!(fresh["guidance"], created["guidance"]);
}

/// Duplicate transfers attachments with native provenance; lost responses recover without altering the original's record.
#[tokio::test]
async fn duplicate_transition_recovers_relation_write() {
    let mut f = Fixture::new().await;
    let project = f.project().await;
    let original = f.work("atomic", &project, None).await;
    let duplicate = f.work("atomic", &project, None).await;
    let original_aid = agent_tasks_linear::records::child_id(&original, "state");
    let duplicate_aid = agent_tasks_linear::records::child_id(&duplicate, "state");
    let original_record = f.db.lock().await.attachments[&original_aid].clone();
    let url = format!(
        "https://linear.app/workspace/issue/{}/original",
        f.db.lock().await.issues[&original]["identifier"]
            .as_str()
            .unwrap()
    );
    f.ok(
        "edit_atomic",
        json!({"id":duplicate,"fields":{"reason":"Same request","duplicate_of":url}}),
    )
    .await;
    let request =
        json!({"request_id":id(),"id":duplicate,"status":"Duplicate","actor_role":"orchestrator"});
    f.db.lock().await.lose = Some("MCreateIssueRelation".into());
    assert_eq!(
        f.call("move_status", request.clone()).await.status,
        "outcome_unknown"
    );
    assert_eq!(
        f.db.lock().await.issues[&duplicate]["state"]["name"],
        "Duplicate"
    );
    f.restart();
    f.ok("move_status", request.clone()).await;
    f.ok("move_status", request).await;
    let context = f
        .ok("get_context", json!({"type":"issue","id":duplicate}))
        .await;
    assert!(context["workflow"]["pending"].is_null());
    assert_eq!(context["discrepancies"], json!([]));
    assert_eq!(context["fields"]["duplicate_of"], url);
    assert_eq!(context["fields"]["reason"], "Same request");
    let original_context = f
        .ok("get_context", json!({"type":"issue","id":original}))
        .await;
    assert_eq!(original_context["discrepancies"], json!([]));
    assert_eq!(
        original_context["workflow"],
        original_record["metadata"]["workflow"]
    );
    let db = f.db.lock().await;
    assert_eq!(db.relations.len(), 1);
    let relation = db.relations.values().next().unwrap();
    assert_eq!(relation["issue"]["id"], duplicate);
    assert_eq!(relation["relatedIssue"]["id"], original);
    assert_eq!(db.issues[&original]["state"]["name"], "Backlog");
    assert_eq!(db.attachments[&original_aid], original_record);
    assert_eq!(db.attachments[&duplicate_aid]["issue"]["id"], original);
    assert_eq!(
        db.attachments[&duplicate_aid]["originalIssue"]["id"],
        duplicate
    );
}

/// Canonical attachment IDs never authorize reading or overwriting another issue's native provenance.
#[tokio::test]
async fn state_attachment_rejects_foreign_provenance_on_read_and_write() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let source = f.work("atomic", &project, None).await;
    let foreign = f.work("atomic", &project, None).await;
    let work = f.gateway.store.work(&source).await.unwrap();
    let aid = agent_tasks_linear::records::child_id(&source, "state");
    for (current, original) in [
        (foreign.as_str(), serde_json::Value::Null),
        (source.as_str(), json!({"id":foreign})),
    ] {
        {
            let mut db = f.db.lock().await;
            let attachment = db.attachments.get_mut(&aid).unwrap();
            attachment["issue"] = json!({"id":current});
            attachment["originalIssue"] = original;
        }
        let before = f.db.lock().await.attachments.clone();
        assert_eq!(
            f.gateway.store.meta(&source).await.unwrap_err().code,
            "STATE_INVALID"
        );
        assert_eq!(
            f.gateway
                .store
                .save(&work.native, work.managed().unwrap())
                .await
                .unwrap_err()
                .code,
            "STATE_INVALID"
        );
        assert_eq!(f.db.lock().await.attachments, before);
    }
}

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
    f.ok("edit_atomic",json!({"id":child,"fields":{"reason":"Duplicate request","duplicate_of":format!("https://linear.app/issue/{module}")}})).await;
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
    let recorded = f.ok("record_review", review.clone()).await;
    assert!(recorded["url"].as_str().unwrap().contains("#comment-"));
    let replayed = f.ok("record_review", review.clone()).await;
    assert_eq!(recorded["url"], replayed["url"]);
    let activity = f.ok("get_comment", json!({"id":recorded["url"]})).await;
    assert_eq!(activity["activity"]["kind"], "review");
    assert_eq!(activity["activity"]["formal_review"], true);
    assert_eq!(activity["activity"]["verdict"], "accepted");
    let ordinary = f
        .ok(
            "add_comment",
            json!({"target_type":"issue","target_id":atom,"body":"accepted"}),
        )
        .await;
    assert_eq!(
        f.ok("get_comment", json!({"id":ordinary["comment"]["id"]}))
            .await["activity"]["formal_review"],
        false
    );
    assert_eq!(
        f.ok("get_context", json!({"type":"issue","id":atom})).await["workflow"]["review"]["id"],
        review["request_id"]
    );
    let records = agent_tasks_linear::activity::read_activity(&f.gateway.store, "issue", &atom)
        .await
        .unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records.iter().filter(|r| r.formal_review).count(), 1);
    assert_eq!(f.db.lock().await.comments.len(), 2);
    f.mv(&atom, "In Progress").await;
    let history = agent_tasks_linear::activity::read_activity(&f.gateway.store, "issue", &atom)
        .await
        .unwrap();
    assert_eq!(history.len(), 2);
    assert!(history.iter().all(|r| !r.formal_review));
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

/// Verify title/priority edits bypass validation of persisted legacy fields without weakening content validation.
#[tokio::test]
async fn presentation_edits_preserve_legacy_metadata_without_adopting_it() {
    let f = Fixture::new().await;
    let project = f.project().await;
    let module = f.work("module", &project, None).await;
    f.result("module", &module).await;
    f.mv(&module, "In Progress").await;
    f.mv(&module, "In Review").await;
    f.review(&module, "accepted").await;
    f.ok(
        "edit_module",
        json!({"id":module,"fields":{"merge_report":"Merged"}}),
    )
    .await;
    f.mv(&module, "Done").await;
    let legacy_pr = "[https://example.com/mcp-fixtures/pull-request](<https://example.com/mcp-fixtures/pull-request>)";
    let legacy_artifact =
        "[https://example.com/mcp-fixtures/artifact](<https://example.com/mcp-fixtures/artifact>)";
    {
        let mut db = f.db.lock().await;
        let attachment = db
            .attachments
            .values_mut()
            .find(|a| a["issue"]["id"] == module)
            .unwrap();
        attachment["metadata"]["workflow"]["fields"]["pr_url"] = json!(legacy_pr);
        attachment["metadata"]["workflow"]["fields"]["artifact_url"] = json!(legacy_artifact);
    }
    let before = f
        .ok("get_context", json!({"type":"issue","id":module}))
        .await;
    f.ok(
        "edit_module",
        json!({"id":module,"title":"Renamed","priority":4}),
    )
    .await;
    let after = f
        .ok("get_context", json!({"type":"issue","id":module}))
        .await;
    assert_eq!(after["issue"]["title"], "[MODULE] Renamed");
    assert_eq!(
        after["issue"]["description"],
        before["issue"]["description"]
    );
    assert_eq!(after["fields"]["pr_url"], legacy_pr);
    assert_eq!(after["fields"]["artifact_url"], legacy_artifact);
    assert_eq!(
        after["workflow"]["revision"],
        before["workflow"]["revision"]
    );
    assert_eq!(after["workflow"]["review"], before["workflow"]["review"]);
    assert_eq!(after["workflow"]["status"], "Done");

    f.mv(&module, "In Progress").await;
    {
        let mut db = f.db.lock().await;
        let attachment = db
            .attachments
            .values_mut()
            .find(|a| a["issue"]["id"] == module)
            .unwrap();
        attachment["metadata"]["workflow"]["fields"]["pr_url"] = json!(legacy_pr);
        attachment["metadata"]["workflow"]["fields"]["artifact_url"] = json!(legacy_artifact);
    }
    let rejected = f
        .call(
            "edit_module",
            json!({"id":module,"fields":{"pr_url":"not a URL"}}),
        )
        .await;
    assert_eq!(rejected.status, "blocked");
    assert_eq!(rejected.data["code"], "INVALID_INPUT");
}
