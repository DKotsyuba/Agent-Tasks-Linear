//! Pure guidance cases over directly constructed managed work records.
//! No Linear transport is involved: every case fixes a complete Project graph in memory and
//! checks the advisory projection against the same guards the mutations enforce.
use agent_tasks::git::GitCommit;
use agent_tasks::guidance::{guidance, preview_effects};
use agent_tasks::model::{Kind, LocalGitReport, Meta, Pending, Review, Status, Work};
use agent_tasks::rules;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Completion timestamp used for every Done fixture so completion identities stay stable.
const DONE_AT: &str = "2026-01-01T00:00:00.000Z";

/// In-memory complete Project graph producing managed work with consistent native data.
struct Fixture {
    /// Graph entries in creation order; parents always precede their children.
    works: Vec<Work>,
}
impl Fixture {
    /// Create an empty graph.
    fn new() -> Self {
        Self { works: vec![] }
    }
    /// Add managed work whose parent is referenced by its graph index.
    fn add(&mut self, kind: Kind, status: Status, parent: Option<usize>, fields: Value) -> usize {
        let id = uuid::Uuid::new_v4().to_string();
        let parent_id = parent.map(|p| self.id(p).to_owned());
        let identifier = format!("F-{}", self.works.len() + 1);
        let completed_at = (status == Status::Done).then(|| DONE_AT.to_owned());
        let native = json!({
            "id": id,
            "identifier": identifier,
            "title": format!("[{}] Fixture", kind.label()),
            "url": format!("https://linear.app/test/issue/{id}"),
            "state": {"name": status.name()},
            "parent": parent_id.as_deref().map(|p| json!({"id": p})),
            "project": {"id": "project"},
            "team": {"id": "team"},
            "labels": {"nodes": [{"name": kind.label()}], "pageInfo": {"hasNextPage": false}},
            "description": "",
            "completedAt": completed_at,
        });
        let meta = Meta {
            schema: 2,
            kind,
            fields: fields.clone(),
            project_id: "project".into(),
            parent_id,
            children: vec![],
            status,
            round: u64::from(matches!(
                status,
                Status::InProgress | Status::InReview | Status::Done
            )),
            revision: 0,
            frozen_modules: None,
            integration: BTreeMap::new(),
            git_reports: vec![],
            review: None,
            completed_at,
            description: String::new(),
            creation: json!({"tool": "fixture"}),
            last_request: None,
            pending: None,
        };
        self.works.push(Work {
            native,
            meta: Some(meta),
            fields,
        });
        self.works.len() - 1
    }
    /// Native UUID of one graph entry.
    fn id(&self, index: usize) -> &str {
        self.works[index].id()
    }
    /// Human identifier of one graph entry.
    fn identifier(&self, index: usize) -> &str {
        self.works[index].native["identifier"].as_str().unwrap()
    }
    /// Mutable metadata of one graph entry for state the builder does not model directly.
    fn meta_mut(&mut self, index: usize) -> &mut Meta {
        self.works[index].meta.as_mut().unwrap()
    }
    /// Set one readable field on both the metadata and the loaded view of a graph entry.
    fn set_field(&mut self, index: usize, key: &str, value: Value) {
        let work = &mut self.works[index];
        work.fields[key] = value.clone();
        work.meta.as_mut().unwrap().fields[key] = value;
    }
    /// Move a fixture entry to a status, updating native state, recorded status, completion
    /// stamp and round exactly as an explicit transition would.
    fn set_status(&mut self, index: usize, status: Status) {
        let work = &mut self.works[index];
        work.native["state"]["name"] = json!(status.name());
        let m = work.meta.as_mut().unwrap();
        m.status = status;
        m.completed_at = (status == Status::Done).then(|| DONE_AT.to_owned());
        work.native["completedAt"] = json!(m.completed_at);
        if status == Status::InProgress {
            m.round += 1;
            m.review = None;
        }
    }
    /// Complete graph slice for guidance and guard calls.
    fn graph(&self) -> &[Work] {
        &self.works
    }
}

/// Readable fields that let a Module start: contracts, checkout identity and local checks.
fn module_fields() -> Value {
    json!({
        "expected_result": "Deliver",
        "acceptance_criteria": "Guards agree",
        "lead": "fixture",
        "branch": "main",
        "worktree": "/tmp/fixture",
        "required_contract": "Accepted decomposition",
        "provided_contract": "Pure projection",
        "repository_url": "https://git.example.test/product"
    })
}

/// Fields of a finished, reviewed and merged Module.
fn merged_module_fields() -> Value {
    let mut fields = module_fields();
    fields["result"] = json!("Implemented");
    fields["check_result"] = json!("Verified");
    fields["pr_url"] = json!("https://example.test/pull/1");
    fields["merge_report"] = json!("Merged");
    fields
}

/// Readable fields that let a Task start and report a non-code result.
fn task_fields() -> Value {
    json!({
        "expected_result": "Deliver",
        "acceptance_criteria": "Guards agree",
        "local_check": "cargo test"
    })
}

/// Fields of an integration Atomic over the supplied participating module IDs.
fn integration_fields(ids: &[String]) -> Value {
    json!({
        "work_type": "integration",
        "integration_modules": ids,
        "scenarios": "Combined request",
        "environment": "Combined checkout",
        "executor": "fixture",
        "local_check": "cargo test",
        "expected_result": "Seam passes",
        "acceptance_criteria": "Guards agree",
        "result": "Seam passed",
        "check_result": "Verified"
    })
}

/// One valid Conventional Commit snapshot as the commit import would record it.
fn git_commit(sha: &str) -> GitCommit {
    GitCommit {
        repository_identity: "/tmp/fixture/.git".into(),
        repository_path: "/tmp/fixture".into(),
        sha: sha.into(),
        subject: "feat(fixture): deliver".into(),
        original_message: "feat(fixture): deliver".into(),
        result: "Delivered".into(),
        checks: "cargo test".into(),
        notes: None,
        author: "Fixture <fixture@example.test>".into(),
        authored_at: "2026-01-01T00:00:00+00:00".into(),
    }
}

/// One recorded verdict matching the supplied round and revision when accepted.
fn review(round: u64, revision: u64, accepted: bool) -> Review {
    Review {
        id: format!("review-{round}-{revision}"),
        round,
        revision,
        accepted,
    }
}

/// Walk preparation, working, review, merge, closure and done stages with the guards' own
/// conditions, never presenting an accepted review or a merged Module as finished early.
#[test]
fn guidance_cases_across_preparation_working_review_and_closure() {
    let mut f = Fixture::new();
    let epic = f.add(
        Kind::Epic,
        Status::InProgress,
        None,
        json!({"expected_result":"Deliver","acceptance_criteria":"Guards agree","business_requirements":"B","result":"Delivered"}),
    );
    // Draft work waits for planning; the move itself is unconditional once drift is clear.
    let draft = f.add(Kind::Module, Status::Backlog, None, module_fields());
    let g = guidance(&f.works[draft], f.graph());
    assert_eq!(g["work_id"], f.id(draft));
    assert_eq!(g["stage"], "preparation");
    assert_eq!(
        g["next_action"],
        json!({"kind":"prepare_work","actor_role":"orchestrator","tool":"move_status","target_status":"Todo"})
    );
    assert_eq!(g["conditions"], json!([]));
    // Planned work missing its start fields prepares them through the kind's edit tool.
    let bare = f.add(Kind::Module, Status::Todo, None, json!({}));
    let g = guidance(&f.works[bare], f.graph());
    assert_eq!(g["stage"], "preparation");
    assert_eq!(g["next_action"]["kind"], "prepare_work");
    assert_eq!(g["next_action"]["tool"], "edit_module");
    assert!(
        g["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c.as_str().unwrap().contains("Required field: lead"))
    );
    // A ready Module under an active Epic starts; only the orchestrator starts it.
    let ready = f.add(Kind::Module, Status::Todo, Some(epic), module_fields());
    let g = guidance(&f.works[ready], f.graph());
    assert_eq!(
        g["next_action"],
        json!({"kind":"start_work","actor_role":"orchestrator","tool":"move_status","target_status":"In Progress"})
    );
    assert_eq!(g["conditions"], json!([]));
    // Code work without an imported current-round snapshot imports commits first.
    let module = f.add(Kind::Module, Status::InProgress, Some(epic), {
        let mut fields = module_fields();
        fields["result"] = json!("Implemented");
        fields["check_result"] = json!("Verified");
        fields["pr_url"] = json!("https://example.test/pull/1");
        fields
    });
    let task = f.add(
        Kind::Task,
        Status::InProgress,
        Some(module),
        json!({"work_type":"code","result":"Implemented","check_result":"Verified","expected_result":"Deliver","acceptance_criteria":"Guards agree","local_check":"cargo test"}),
    );
    let g = guidance(&f.works[task], f.graph());
    assert_eq!(g["stage"], "working");
    assert_eq!(
        g["next_action"],
        json!({"kind":"attach_commits","actor_role":"worker","tool":"record_commits","target_status":null})
    );
    assert_eq!(g["conditions"], json!(["Required field: commit_url"]));
    // With the snapshot imported the Task closes locally.
    f.meta_mut(task).git_reports = vec![LocalGitReport {
        round: 1,
        commit: git_commit("a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"),
    }];
    let g = guidance(&f.works[task], f.graph());
    assert_eq!(
        g["next_action"],
        json!({"kind":"close_work","actor_role":"worker","tool":"move_status","target_status":"Done"})
    );
    // A Module with an unfinished child has no single action of its own; the child guides.
    let g = guidance(&f.works[module], f.graph());
    assert_eq!(g["stage"], "working");
    assert!(g["next_action"].is_null());
    assert!(
        g["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c.as_str().unwrap().starts_with("Child is unfinished"))
    );
    f.set_status(task, Status::Done);
    let g = guidance(&f.works[module], f.graph());
    assert_eq!(
        g["next_action"],
        json!({"kind":"request_review","actor_role":"worker","tool":"move_status","target_status":"In Review"})
    );
    // In Review without a verdict the reviewer records one.
    f.set_status(module, Status::InReview);
    let g = guidance(&f.works[module], f.graph());
    assert_eq!(g["stage"], "review");
    assert_eq!(g["next_action"]["kind"], "record_review");
    assert_eq!(g["next_action"]["actor_role"], "reviewer");
    assert_eq!(g["next_action"]["tool"], "record_review");
    // Requested changes are applied by the lead; no single call performs fixing.
    f.meta_mut(module).review = Some(review(1, 0, false));
    let g = guidance(&f.works[module], f.graph());
    assert_eq!(g["stage"], "fixes");
    assert_eq!(
        g["next_action"],
        json!({"kind":"apply_fixes","actor_role":"worker","tool":null,"target_status":null})
    );
    // An accepted current review without a merge report keeps an explicit reporting action.
    f.meta_mut(module).review = Some(review(1, 0, true));
    let g = guidance(&f.works[module], f.graph());
    assert_eq!(g["stage"], "merge");
    assert_eq!(
        g["next_action"],
        json!({"kind":"record_merge","actor_role":"orchestrator","tool":"edit_module","target_status":null})
    );
    assert_eq!(g["conditions"], json!(["Required field: merge_report"]));
    // With the merge reported the orchestrator closes.
    f.set_field(module, "merge_report", json!("Merged in production"));
    let g = guidance(&f.works[module], f.graph());
    assert_eq!(g["stage"], "closure");
    assert_eq!(
        g["next_action"],
        json!({"kind":"close_work","actor_role":"orchestrator","tool":"move_status","target_status":"Done"})
    );
    assert_eq!(g["conditions"], json!([]));
    // A verdict older than the submitted revision is stale and needs a fresh review.
    f.meta_mut(module).review = Some(review(1, 1, true));
    let g = guidance(&f.works[module], f.graph());
    assert_eq!(g["stage"], "review");
    assert_eq!(g["next_action"]["kind"], "record_review");
    f.meta_mut(module).review = Some(review(1, 0, true));
    // Finished work guides nowhere further.
    f.set_status(module, Status::Done);
    let g = guidance(&f.works[module], f.graph());
    assert_eq!(g["stage"], "done");
    assert!(g["next_action"].is_null());
    assert_eq!(g["conditions"], json!([]));
    // An Epic delivering several merged Modules needs current integration coverage first.
    let second = f.add(
        Kind::Module,
        Status::Done,
        Some(epic),
        merged_module_fields(),
    );
    f.meta_mut(second).review = Some(review(1, 0, true));
    let retired = f.add(
        Kind::Atomic,
        Status::Canceled,
        Some(epic),
        json!({"reason":"Scope dropped"}),
    );
    let g = guidance(&f.works[epic], f.graph());
    assert_eq!(g["stage"], "working");
    assert_eq!(
        g["next_action"],
        json!({"kind":"run_integration","actor_role":"orchestrator","tool":null,"target_status":null})
    );
    assert!(
        g["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c.as_str().unwrap().contains("integration coverage"))
    );
    // Retired work is excluded from further guidance.
    let g = guidance(&f.works[retired], f.graph());
    assert_eq!(g["stage"], "excluded");
    assert!(g["next_action"].is_null());
    assert_eq!(g["conditions"], json!([]));
    // Ordinary unmanaged issues report their condition with no workflow action.
    let plain = Work {
        native: json!({"id":"plain","state":{"name":"Todo"},"parent":null,"project":{"id":"project"}}),
        meta: None,
        fields: json!({}),
    };
    let g = guidance(&plain, f.graph());
    assert_eq!(g["work_id"], "plain");
    assert_eq!(g["stage"], "preparation");
    assert!(g["next_action"].is_null());
    assert_eq!(g["conditions"], json!(["Issue has no MCP workflow data"]));
}

/// Pending writes, native drift and stale integrations guide to recovery, never to success.
#[test]
fn guidance_cases_for_recovery_pending_drift_and_stale_integration() {
    let mut f = Fixture::new();
    // A pending write names its exact retry tool and the role that may repeat it.
    let module = f.add(Kind::Module, Status::Todo, None, module_fields());
    let request = json!({"tool":"edit_module","arguments":{"id":f.id(module),"fields":{"branch":"feature/x"},"actor":"lead"}});
    let held = f.works[module].meta.clone().unwrap();
    f.meta_mut(module).pending = Some(Box::new(Pending {
        request,
        next: held,
        input: json!({}),
        before: json!({}),
    }));
    let g = guidance(&f.works[module], f.graph());
    assert_eq!(g["stage"], "recovery");
    assert_eq!(
        g["next_action"],
        json!({"kind":"retry_operation","actor_role":"orchestrator","tool":"edit_module","target_status":null})
    );
    assert!(
        g["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c.as_str().unwrap().contains("pending"))
    );
    // A native status moved away from the record recovers through the explicit reopen.
    let drifted = f.add(Kind::Task, Status::InProgress, Some(module), task_fields());
    f.works[drifted].native["state"]["name"] = json!("Todo");
    let g = guidance(&f.works[drifted], f.graph());
    assert_eq!(g["stage"], "recovery");
    assert_eq!(
        g["next_action"],
        json!({"kind":"start_work","actor_role":"worker","tool":"move_status","target_status":"In Progress"})
    );
    // Structural drift has no single resolving call; the conditions stay visible.
    let edited = f.add(Kind::Task, Status::InProgress, Some(module), task_fields());
    f.meta_mut(edited).description = "changed outside the workflow".into();
    let g = guidance(&f.works[edited], f.graph());
    assert_eq!(g["stage"], "recovery");
    assert!(g["next_action"].is_null());
    assert!(
        g["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c.as_str().unwrap().contains("Description changed"))
    );
    // A completed integration whose module snapshot went stale must run again.
    let epic = f.add(
        Kind::Epic,
        Status::InProgress,
        None,
        json!({"expected_result":"Deliver","acceptance_criteria":"Guards agree","business_requirements":"B"}),
    );
    let merged = f.add(
        Kind::Module,
        Status::Done,
        Some(epic),
        merged_module_fields(),
    );
    f.meta_mut(merged).review = Some(review(1, 0, true));
    let merged2 = f.add(
        Kind::Module,
        Status::Done,
        Some(epic),
        merged_module_fields(),
    );
    f.meta_mut(merged2).review = Some(review(1, 0, true));
    let seam = f.add(
        Kind::Atomic,
        Status::Done,
        Some(epic),
        integration_fields(&[f.id(merged).to_owned(), f.id(merged2).to_owned()]),
    );
    f.meta_mut(seam).review = Some(review(1, 0, true));
    f.meta_mut(seam).integration = BTreeMap::from([
        (f.id(merged).to_owned(), "0:0:stale".to_owned()),
        (
            f.id(merged2).to_owned(),
            rules::completion(&f.works[merged2]),
        ),
    ]);
    let g = guidance(&f.works[seam], f.graph());
    assert_eq!(g["stage"], "recovery");
    assert_eq!(
        g["next_action"],
        json!({"kind":"run_integration","actor_role":"orchestrator","tool":"move_status","target_status":"In Progress"})
    );
    assert_eq!(g["conditions"], json!([]));
    // A current integration over the same Modules is simply finished.
    f.meta_mut(seam).integration = BTreeMap::from([
        (f.id(merged).to_owned(), rules::completion(&f.works[merged])),
        (
            f.id(merged2).to_owned(),
            rules::completion(&f.works[merged2]),
        ),
    ]);
    let g = guidance(&f.works[seam], f.graph());
    assert_eq!(g["stage"], "done");
    assert!(g["next_action"].is_null());
}

/// Every advertised move passes the same guards for its advertised role across a mixed graph.
#[test]
fn guidance_cases_agree_with_transition_guards() {
    let mut f = Fixture::new();
    let epic = f.add(
        Kind::Epic,
        Status::InProgress,
        None,
        json!({"expected_result":"Deliver","acceptance_criteria":"Guards agree","business_requirements":"B","result":"Delivered"}),
    );
    let planned = f.add(Kind::Module, Status::Todo, Some(epic), module_fields());
    let active = f.add(
        Kind::Module,
        Status::InProgress,
        Some(epic),
        merged_module_fields(),
    );
    let task = f.add(
        Kind::Task,
        Status::InProgress,
        Some(active),
        json!({"work_type":"code","result":"Implemented","check_result":"Verified","expected_result":"Deliver","acceptance_criteria":"Guards agree","local_check":"cargo test"}),
    );
    f.meta_mut(task).git_reports = vec![LocalGitReport {
        round: 1,
        commit: git_commit("b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4"),
    }];
    let checked = f.add(
        Kind::Atomic,
        Status::InReview,
        Some(active),
        json!({"expected_result":"Deliver","acceptance_criteria":"Guards agree","executor":"fixture","local_check":"cargo test","artifact_url":"https://example.test/report","result":"Delivered","check_result":"Verified"}),
    );
    f.meta_mut(checked).review = Some(review(1, 0, true));
    let merged = f.add(
        Kind::Module,
        Status::Done,
        Some(epic),
        merged_module_fields(),
    );
    f.meta_mut(merged).review = Some(review(1, 0, true));
    let merged2 = f.add(
        Kind::Module,
        Status::Done,
        Some(epic),
        merged_module_fields(),
    );
    f.meta_mut(merged2).review = Some(review(1, 0, true));
    let seam = f.add(
        Kind::Atomic,
        Status::Done,
        Some(epic),
        integration_fields(&[f.id(merged).to_owned(), f.id(merged2).to_owned()]),
    );
    f.meta_mut(seam).review = Some(review(1, 0, true));
    f.meta_mut(seam).integration = BTreeMap::from([
        (f.id(merged).to_owned(), rules::completion(&f.works[merged])),
        (
            f.id(merged2).to_owned(),
            rules::completion(&f.works[merged2]),
        ),
    ]);
    let retired = f.add(
        Kind::Atomic,
        Status::Duplicate,
        None,
        json!({"reason":"Same request","duplicate_of":"https://linear.app/test/issue/original"}),
    );
    for work in f.graph() {
        let g = guidance(work, f.graph());
        if let Some(target) = g["next_action"]["target_status"].as_str() {
            assert!(
                g["conditions"].as_array().unwrap().is_empty(),
                "{}: {g}",
                work.id()
            );
            let status: Status = serde_json::from_value(json!(target)).unwrap();
            let role = g["next_action"]["actor_role"].as_str().unwrap();
            let errors = rules::transition(work, f.graph(), status, role);
            assert!(
                errors.is_empty(),
                "{} -> {target} as {role}: {errors:?}",
                work.id()
            );
        }
    }
    // Focused spot checks over the same graph.
    let g = guidance(&f.works[planned], f.graph());
    assert_eq!(g["next_action"]["kind"], "start_work");
    let g = guidance(&f.works[task], f.graph());
    assert_eq!(g["next_action"]["kind"], "close_work");
    let g = guidance(&f.works[checked], f.graph());
    assert_eq!(g["next_action"]["kind"], "close_work");
    assert_eq!(g["next_action"]["actor_role"], "orchestrator");
    let g = guidance(&f.works[retired], f.graph());
    assert_eq!(g["stage"], "excluded");
    assert_eq!(f.identifier(seam), "F-8");
    // Previews agree with the same guards for every status and both trusted roles, and
    // promise effects exactly when the transition is allowed.
    for work in f.graph() {
        for target in rules::STATUSES {
            for role in ["orchestrator", "worker"] {
                let errors = rules::transition(work, f.graph(), target, role);
                let preview = preview_effects(work, f.graph(), target, role);
                assert_eq!(preview["allowed"], json!(errors.is_empty()));
                assert_eq!(preview["conditions"], json!(errors));
                assert_eq!(preview["effects"].is_object(), errors.is_empty());
            }
        }
    }
}

/// Previews mirror the mutation path exactly: blocked transitions show conditions only,
/// first starts and no-ops change nothing, reopens clear recorded results and invalidate
/// only currently valid integration snapshots.
#[test]
fn preview_cases_mirror_execution_effects() {
    let mut f = Fixture::new();
    let epic = f.add(
        Kind::Epic,
        Status::InProgress,
        None,
        json!({"expected_result":"Deliver","acceptance_criteria":"Guards agree","business_requirements":"B"}),
    );
    // A blocked transition keeps its conditions and never promises effects.
    let bare = f.add(Kind::Module, Status::Todo, Some(epic), json!({}));
    let p = preview_effects(
        &f.works[bare],
        f.graph(),
        Status::InProgress,
        "orchestrator",
    );
    assert_eq!(p["allowed"], json!(false));
    assert!(p["effects"].is_null());
    assert!(
        p["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c.as_str().unwrap().contains("Required field: lead"))
    );
    // A first start advances the round but clears nothing.
    let fresh = f.add(Kind::Module, Status::Todo, Some(epic), module_fields());
    let p = preview_effects(
        &f.works[fresh],
        f.graph(),
        Status::InProgress,
        "orchestrator",
    );
    assert_eq!(p["allowed"], json!(true));
    assert_eq!(p["conditions"], json!([]));
    assert_eq!(
        p["effects"],
        json!({"clears": [], "review_invalidated": false, "affected_integrations": [], "round_changes": true})
    );
    // A same-state repeat is a no-op with empty effects.
    let active = f.add(
        Kind::Module,
        Status::InProgress,
        Some(epic),
        module_fields(),
    );
    let p = preview_effects(
        &f.works[active],
        f.graph(),
        Status::InProgress,
        "orchestrator",
    );
    assert_eq!(p["allowed"], json!(true));
    assert_eq!(
        p["effects"],
        json!({"clears": [], "review_invalidated": false, "affected_integrations": [], "round_changes": false})
    );
    // Reopening a merged Module clears its recorded results and the completion stamp,
    // invalidates its review and reports only the still-valid integration snapshot.
    let done = f.add(
        Kind::Module,
        Status::Done,
        Some(epic),
        merged_module_fields(),
    );
    f.meta_mut(done).review = Some(review(1, 0, true));
    let other = f.add(
        Kind::Module,
        Status::Done,
        Some(epic),
        merged_module_fields(),
    );
    f.meta_mut(other).review = Some(review(1, 0, true));
    let current = f.add(
        Kind::Atomic,
        Status::Done,
        Some(epic),
        integration_fields(&[f.id(done).to_owned(), f.id(other).to_owned()]),
    );
    f.meta_mut(current).review = Some(review(1, 0, true));
    f.meta_mut(current).integration = BTreeMap::from([
        (f.id(done).to_owned(), rules::completion(&f.works[done])),
        (f.id(other).to_owned(), rules::completion(&f.works[other])),
    ]);
    let stale = f.add(
        Kind::Atomic,
        Status::Done,
        Some(epic),
        integration_fields(&[f.id(done).to_owned(), f.id(other).to_owned()]),
    );
    f.meta_mut(stale).review = Some(review(1, 0, true));
    f.meta_mut(stale).integration = BTreeMap::from([
        (f.id(done).to_owned(), "0:0:old".to_owned()),
        (f.id(other).to_owned(), rules::completion(&f.works[other])),
    ]);
    let p = preview_effects(
        &f.works[done],
        f.graph(),
        Status::InProgress,
        "orchestrator",
    );
    assert_eq!(p["allowed"], json!(true));
    assert_eq!(
        p["effects"]["clears"],
        json!([
            "result",
            "check_result",
            "commit_url",
            "artifact_url",
            "merge_report",
            "pr_url",
            "completed_at"
        ])
    );
    assert_eq!(p["effects"]["review_invalidated"], json!(true));
    assert_eq!(p["effects"]["round_changes"], json!(true));
    assert_eq!(
        p["effects"]["affected_integrations"],
        json!([f.identifier(current)])
    );
    // Retiring the same merged Module also invalidates only the current snapshot.
    f.set_field(done, "reason", json!("Scope retired"));
    let p = preview_effects(&f.works[done], f.graph(), Status::Canceled, "orchestrator");
    assert_eq!(p["allowed"], json!(true));
    assert_eq!(
        p["effects"],
        json!({"clears": [], "review_invalidated": false, "affected_integrations": [f.identifier(current)], "round_changes": false})
    );
}

/// A Module submission that rebinds its derived result invalidates the recorded review
/// without touching the round, and an integration restart clears its own results only.
#[test]
fn preview_cases_for_submission_rebind_and_integration_restart() {
    let mut f = Fixture::new();
    let epic = f.add(
        Kind::Epic,
        Status::InProgress,
        None,
        json!({"expected_result":"Deliver","acceptance_criteria":"Guards agree","business_requirements":"B"}),
    );
    // Submission with a changed derived report drops the recorded review.
    let module = f.add(Kind::Module, Status::InProgress, Some(epic), {
        let mut fields = module_fields();
        fields["pr_url"] = json!("https://example.test/pull/1");
        fields
    });
    // The Done child supplies the derived submission report the Module rebinds to.
    let _task = f.add(
        Kind::Task,
        Status::Done,
        Some(module),
        json!({"expected_result":"Deliver","acceptance_criteria":"Guards agree","local_check":"cargo test","artifact_url":"https://example.test/report","result":"Child result","check_result":"Child checks"}),
    );
    f.meta_mut(module).review = Some(review(1, 0, true));
    let p = preview_effects(&f.works[module], f.graph(), Status::InReview, "worker");
    assert_eq!(p["allowed"], json!(true));
    assert_eq!(
        p["effects"],
        json!({"clears": [], "review_invalidated": true, "affected_integrations": [], "round_changes": false})
    );
    // Restarting an active integration after a module change clears its own recorded
    // results and advances the round; no integration references the seam itself.
    let done = f.add(
        Kind::Module,
        Status::Done,
        Some(epic),
        merged_module_fields(),
    );
    f.meta_mut(done).review = Some(review(1, 0, true));
    let other = f.add(
        Kind::Module,
        Status::Done,
        Some(epic),
        merged_module_fields(),
    );
    f.meta_mut(other).review = Some(review(1, 0, true));
    let seam = f.add(
        Kind::Atomic,
        Status::InProgress,
        Some(epic),
        integration_fields(&[f.id(done).to_owned(), f.id(other).to_owned()]),
    );
    f.meta_mut(seam).round = 2;
    f.meta_mut(seam).integration = BTreeMap::from([
        (f.id(done).to_owned(), "0:0:old".to_owned()),
        (f.id(other).to_owned(), rules::completion(&f.works[other])),
    ]);
    let p = preview_effects(
        &f.works[seam],
        f.graph(),
        Status::InProgress,
        "orchestrator",
    );
    assert_eq!(p["allowed"], json!(true));
    assert_eq!(
        p["effects"],
        json!({"clears": ["result", "check_result", "commit_url", "artifact_url", "merge_report"], "review_invalidated": false, "affected_integrations": [], "round_changes": true})
    );
}
