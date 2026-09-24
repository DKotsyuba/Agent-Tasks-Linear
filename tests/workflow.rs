//! End-to-end gateway checks against an HTTP fixture acting as durable Linear state.

use agent_tasks_linear::{
    gateway::Gateway,
    linear::Linear,
    model::{Outcome, Principal, Role},
    records::{Signer, Store},
};
use axum::{Json, Router, extract::State, routing::post};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::Mutex;
use uuid::Uuid;

/// External test service state, retained when the application gateway is recreated.
#[derive(Default)]
struct Fake {
    /// Native issues keyed by reserved UUID.
    issues: BTreeMap<String, Value>,
    /// Attachments, including durable workflow receipts.
    attachments: BTreeMap<String, Value>,
    /// Native document content for published snapshots.
    documents: BTreeMap<String, Value>,
    /// Human-readable comments created by workflow operations.
    comments: BTreeMap<String, Value>,
    /// Mutation whose effect should happen while its response is lost once.
    fail_after: Option<String>,
    /// Number of mutations observed, for read-only and duplicate assertions.
    writes: usize,
    /// One earlier valid attachment version returned before the current replicated value.
    stale_attachment: Option<Value>,
    /// Drop one work-head upsert before applying it, leaving the receipt's started step recoverable.
    drop_head_once: bool,
}
/// Fixture boundary: execute a known static GraphQL operation, never arbitrary GraphQL text.
async fn graphql(State(state): State<Arc<Mutex<Fake>>>, Json(request): Json<Value>) -> Json<Value> {
    let mut s = state.lock().await;
    let op = request["operationName"].as_str().unwrap();
    let v = &request["variables"];
    let input = &v["input"];
    let id = v["id"].as_str().unwrap_or("");
    if op == "MUpsertRecord" && input["metadata"]["at_kind"] == "work_head" && s.drop_head_once {
        s.drop_head_once = false;
        return Json(
            json!({"errors":[{"message":"dropped head write","extensions":{"code":"INTERNAL_SERVER_ERROR"}}]}),
        );
    }
    let data = match op {
        "QIssue" => json!({"issue":s.issues.get(id)}),
        "QAttachmentById" => {
            if s.stale_attachment
                .as_ref()
                .is_some_and(|value| value["id"] == id)
            {
                json!({"attachment":s.stale_attachment.take()})
            } else {
                json!({"attachment":s.attachments.get(id)})
            }
        }
        "QDocument" => json!({"document":s.documents.get(id)}),
        "QComment" => json!({"comment":s.comments.get(id)}),
        "QTeam" => {
            json!({"team":{"id":id,"name":"Test","autoCloseParentIssues":false,"autoCloseChildIssues":false}})
        }
        "QWorkAttachments" => {
            let values = s
                .attachments
                .values()
                .filter(|v| v["issue"]["id"] == id)
                .cloned()
                .collect::<Vec<_>>();
            let offset = v["after"]
                .as_str()
                .and_then(|x| x.parse::<usize>().ok())
                .unwrap_or(0);
            let end = (offset + 3).min(values.len());
            json!({"issue":{"id":id,"attachments":{"nodes":values[offset..end],"pageInfo":{"hasNextPage":end<values.len(),"endCursor":end.to_string()}}}})
        }
        "MUpsertRecord" => {
            let id = input["id"].as_str().unwrap().to_owned();
            let value = json!({"id":id,"title":input["title"],"url":input["url"],"metadata":input["metadata"],"issue":{"id":input["issueId"]}});
            s.attachments.insert(id, value.clone());
            json!({"attachmentCreate":{"success":true,"attachment":value}})
        }
        "MCreateIssue" => {
            let id = input["id"].as_str().unwrap().to_owned();
            assert!(!s.issues.contains_key(&id), "duplicate logical create");
            let value = json!({"id":id,"identifier":"TEST-1","title":input["title"],"description":input["description"],"url":format!("https://linear.app/test/issue/{id}"),"team":{"id":input["teamId"]},"project":{"id":input["projectId"]},"parent":if input["parentId"].is_null(){Value::Null}else{json!({"id":input["parentId"]})},"state":{"id":input["stateId"]},"archivedAt":null});
            s.issues.insert(id, value.clone());
            json!({"issueCreate":{"success":true,"issue":value}})
        }
        "MUpdateIssue" => {
            let value = s.issues.get_mut(id).unwrap();
            for (field, key) in [
                ("stateId", "state"),
                ("assigneeId", "assignee"),
                ("delegateId", "delegate"),
            ] {
                if input[field].is_string() {
                    value[key] = json!({"id":input[field]});
                }
            }
            json!({"issueUpdate":{"success":true,"issue":value}})
        }
        "MCreateDocument" => {
            let id = input["id"].as_str().unwrap().to_owned();
            assert!(!s.documents.contains_key(&id));
            let value = json!({"id":id,"title":input["title"],"content":input["content"],"url":format!("https://linear.app/doc/{id}"),"project":{"id":input["projectId"]},"archivedAt":null});
            s.documents.insert(id, value.clone());
            json!({"documentCreate":{"success":true,"document":value}})
        }
        "MUpdateDocument" => {
            let value = s.documents.get_mut(id).unwrap();
            value["title"] = input["title"].clone();
            value["content"] = input["content"].clone();
            json!({"documentUpdate":{"success":true,"document":value}})
        }
        "MCreateComment" => {
            let id = input["id"].as_str().unwrap().to_owned();
            let value = json!({"id":id,"body":input["body"],"issue":{"id":input["issueId"]},"user":{"id":"human"}});
            s.comments.insert(id, value.clone());
            json!({"commentCreate":{"success":true,"comment":value}})
        }
        _ => panic!("fixture needs operation {op}"),
    };
    if op.starts_with('M') {
        s.writes += 1;
    }
    if s.fail_after.as_deref() == Some(op) {
        s.fail_after = None;
        return Json(
            json!({"data":data,"errors":[{"message":"injected response loss","extensions":{"code":"INTERNAL_SERVER_ERROR"}}]}),
        );
    }
    Json(json!({"data":data}))
}

/// Ready product and its authenticated owner plus access to the external fixture.
struct Fixture {
    /// Gateway under test.
    g: Arc<Gateway>,
    /// Protected owner binding.
    owner: Principal,
    /// Product UUID.
    product: String,
    /// External simulated Linear state.
    state: Arc<Mutex<Fake>>,
    /// HTTP service task stopped at fixture teardown.
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    /// Stop only this test's isolated HTTP service.
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    /// Seed a signed product into an external HTTP fixture, without a local application database.
    async fn new() -> Self {
        let state = Arc::new(Mutex::new(Fake::default()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/graphql", post(graphql))
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let product = Uuid::new_v4().to_string();
        let project = Uuid::new_v4().to_string();
        let team = Uuid::new_v4().to_string();
        let draft = Uuid::new_v4().to_string();
        let base = format!("https://linear.app/test/issue/{product}");
        let owner = Principal {
            id: "owner".into(),
            role: Role::Owner,
            products: vec![],
            assignment_id: None,
            generation: None,
            epoch: 1,
        };
        state.lock().await.issues.insert(product.clone(),json!({"id":product,"title":"Product","url":base,"project":{"id":project},"team":{"id":team},"parent":null,"state":{"id":draft},"archivedAt":null}));
        let store = Store {
            linear: Linear::mock(&format!("http://{address}/graphql")).unwrap(),
            signer: Signer::new("0123456789012345678901234567890123456789").unwrap(),
        };
        let mut states = json!({});
        for name in [
            "draft",
            "ready",
            "in_progress",
            "review",
            "accepted",
            "skipped",
            "cancelled",
        ] {
            states[name] = json!(if name == "draft" {
                draft.clone()
            } else {
                Uuid::new_v4().to_string()
            });
        }
        for (kind, payload) in [
            (
                "product_config",
                json!({"general_project_id":project,"record_base_url":base,"team_ids":[team],"epoch":1,"product_state":"active","policy_version":1,"issue_state_ids":states,"pending_operation_key":null,"policy":{"integration_uses_owner_pat":true,"allow_root_attributed_owner_decisions":false}}),
            ),
            (
                "identity",
                json!({"kind":"product","primary_parent_id":null,"native_project_id":project,"native_parent_id":null,"record_base_url":base}),
            ),
            (
                "work_head",
                json!({"state":"draft","native_state_id":draft,"children":[],"assignment_ids":[]}),
            ),
        ] {
            let record = store
                .signer
                .record(&owner, &product, &product, kind, payload, None)
                .unwrap();
            store.put(&record, &base).await.unwrap();
        }
        let g = Gateway::new(store).unwrap();
        Self {
            g,
            owner,
            product,
            state,
            task,
        }
    }
    /// Fill exact expected tokens from current external facts for a test intent.
    async fn args(&self, work: &str, p: &Principal, mut body: Value) -> Value {
        let s = self.g.store.snapshot(&self.product).await.unwrap();
        let head = &s.work(work).unwrap().head;
        body["product_id"] = json!(self.product);
        body["idempotency_key"] = json!(Uuid::new_v4().to_string());
        body["expected"] = json!({"work_revision":head.revision});
        if head.payload["plan_hash"].is_string() {
            body["expected"]["plan_hash"] = head.payload["plan_hash"].clone();
        }
        if let Some(g) = p.generation {
            body["expected"]["assignment_generation"] = json!(g);
        }
        body
    }
    /// Execute one expected successful mutation and return its structured outcome.
    async fn write(&self, p: &Principal, name: &str, work: &str, body: Value) -> Outcome {
        let args = self.args(work, p, body).await;
        let result = self.g.call(p, name, args).await;
        assert!(
            matches!(result.status.as_str(), "committed" | "noop"),
            "{name}: {result:?}"
        );
        result
    }
    /// Create a native research work to exercise non-Git semantics.
    async fn create(&self, parent: &str, kind: &str) -> String {
        self.write(&self.owner,"at_work_create",parent,json!({"kind":kind,"parent_id":parent,"title":"Work","description":"Fixture work","classification":"research"})).await.data["created_work_id"].as_str().unwrap().into()
    }
    /// Assign a module with default lead role and construct its separately authenticated binding.
    async fn assign(&self, work: &str) -> Principal {
        let out = self
            .write(
                &self.owner,
                "at_assign",
                work,
                json!({"work_id":work,"principal_id":"lead"}),
            )
            .await;
        Principal {
            id: "lead".into(),
            role: Role::Lead,
            products: vec![self.product.clone()],
            assignment_id: out.data["assignment_id"].as_str().map(str::to_owned),
            generation: Some(1),
            epoch: 1,
        }
    }
    /// Begin work with a reported source location and runtime identity.
    async fn begin(&self, work: &str, lead: &Principal) {
        self.write(lead,"at_begin",work,json!({"work_id":work,"execution":{"repository":"fixture/repo","branch":"codex/activity","worktree":"/fixture/worktree","agent":"lead","runtime":"codex","run_id":"fixture-run","run_url":"https://example.test/runs/fixture-run"}})).await;
    }
}

/// A trusted agent can finish directly and recover its complete activity trace after a new gateway starts.
#[tokio::test]
async fn activity_cycle_records_agent_workspace_and_artifacts_without_proofs() {
    let f = Fixture::new().await;
    let module = f.create(&f.product, "module").await;
    let task = f.create(&module, "task").await;
    let lead = f.assign(&module).await;
    let execution = json!({"repository":"repo:example","branch":"feature/activity","worktree":"/tmp/activity-worktree",
        "agent":"lead","runtime":"native","run_id":"run-1","run_url":"https://example.test/runs/1"});
    let request = |mut body: Value| {
        body["product_id"] = json!(f.product);
        body["idempotency_key"] = json!(Uuid::new_v4().to_string());
        body
    };
    let begun =
        f.g.call(
            &lead,
            "at_begin",
            request(json!({"work_id":task,"execution":execution})),
        )
        .await;
    assert_eq!(begun.status, "committed", "{begun:?}");
    let pr = json!({"kind":"pull_request","locator":"https://example.test/pr/1"});
    f.write(
        &lead,
        "at_checkpoint",
        &task,
        json!({"work_id":task,"summary":"Implemented the change","artifacts":[pr]}),
    )
    .await;
    let done_args = request(json!({"work_id":task,"summary":"Done","artifacts":[
        {"kind":"git_commit","locator":"repo:example@abc1234","commit":"abc1234"},pr]}));
    let done = f.g.call(&lead, "at_complete", done_args.clone()).await;
    assert_eq!(done.status, "committed", "{done:?}");
    assert_eq!(done.data["state"], "accepted");
    let fresh = Gateway::new(f.g.store.clone()).unwrap();
    let context = fresh
        .call(
            &lead,
            "at_context",
            json!({"product_id":f.product,"work_id":task,"section":"work"}),
        )
        .await;
    let content: Value = serde_json::from_str(context.data["content"].as_str().unwrap()).unwrap();
    assert_eq!(content["head"]["result_id"], done.data["result_id"]);
    assert_eq!(content["activity"]["execution"], execution);
    assert_eq!(content["activity"]["summary"], "Done");
    assert_eq!(content["activity"]["completed_by"], "lead");
    assert_eq!(
        content["activity"]["artifacts"].as_array().unwrap().len(),
        2
    );
    let resume = fresh
        .call(
            &lead,
            "at_resume",
            json!({"product_id":f.product,"work_id":task}),
        )
        .await;
    assert_eq!(
        resume.data["items"][0]["activity"]["execution"]["worktree"],
        "/tmp/activity-worktree"
    );
    let facts = fresh.store.snapshot(&f.product).await.unwrap();
    assert!(
        facts
            .records
            .values()
            .any(|r| r.record_kind == "checkpoint" && r.work_id == task)
    );
    assert!(
        facts
            .records
            .values()
            .any(|r| r.record_kind == "result" && r.work_id == task)
    );
    let writes = f.state.lock().await.writes;
    assert_eq!(
        fresh
            .call(&lead, "at_complete", done_args.clone())
            .await
            .data["result_id"],
        done.data["result_id"]
    );
    assert_eq!(f.state.lock().await.writes, writes);
    let mut changed = done_args;
    changed["summary"] = json!("Different result");
    assert_eq!(
        fresh.call(&lead, "at_complete", changed).await.violations[0]["code"],
        "PAYLOAD_MISMATCH"
    );
    let state = f.state.lock().await;
    let comments = state
        .comments
        .values()
        .map(|v| v["body"].as_str().unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        comments.contains("https://example.test/runs/1")
            && comments.contains("https://example.test/pr/1")
    );
    drop(state);
    let restarted = f
        .write(&lead, "at_begin", &task, json!({"work_id":task}))
        .await;
    assert!(restarted.data["activity"]["execution"]["run_id"].is_null());
    assert!(
        restarted.data["activity"]["artifacts"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let revised=f.write(&lead,"at_complete",&task,json!({"work_id":task,"summary":"New run","artifacts":[{"kind":"file","locator":"new-result.txt"}]})).await;
    assert_eq!(
        revised.data["activity"]["artifacts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

/// Review is optional reported activity; direct approval and an explicitly requested review both work.
#[tokio::test]
async fn optional_review_and_retired_children_do_not_block_reported_completion() {
    let f = Fixture::new().await;
    let module = f.create(&f.product, "module").await;
    let child = f.create(&module, "task").await;
    f.write(
        &f.owner,
        "at_work_retire",
        &child,
        json!({"work_id":child,"disposition":"skipped","reason":"No longer needed"}),
    )
    .await;
    let lead = f.assign(&module).await;
    f.begin(&module, &lead).await;
    f.write(
        &lead,
        "at_submit",
        &module,
        json!({"work_id":module,"summary":"Module delivered","artifacts":[]}),
    )
    .await;
    let accepted = f
        .write(
            &f.owner,
            "at_accept",
            &module,
            json!({"work_id":module,"reason":"Recorded result"}),
        )
        .await;
    assert_eq!(accepted.data["state"], "accepted");

    let other = f.create(&f.product, "module").await;
    f.write(
        &f.owner,
        "at_submit",
        &other,
        json!({"work_id":other,"summary":"Please review","artifacts":[]}),
    )
    .await;
    let case = f
        .write(
            &f.owner,
            "at_review_open",
            &other,
            json!({"work_id":other,"reviewer_principal":"reviewer"}),
        )
        .await;
    let repeat = f
        .write(
            &f.owner,
            "at_review_open",
            &other,
            json!({"work_id":other,"reviewer_principal":"reviewer"}),
        )
        .await;
    assert_eq!(
        repeat.data["reviewer_assignment_id"],
        case.data["reviewer_assignment_id"]
    );
    let reviewer = Principal {
        id: "reviewer".into(),
        role: Role::Reviewer,
        products: vec![f.product.clone()],
        assignment_id: case.data["reviewer_assignment_id"]
            .as_str()
            .map(str::to_owned),
        generation: Some(1),
        epoch: 1,
    };
    f.write(
        &reviewer,
        "at_review_report",
        &other,
        json!({"work_id":other,"case_id":case.data["case_id"],"summary":"Checked the behavior"}),
    )
    .await;
    let replacement = f
        .write(
            &f.owner,
            "at_review_open",
            &other,
            json!({"work_id":other,"reviewer_principal":"reviewer-next"}),
        )
        .await;
    assert_eq!(replacement.data["case_id"], case.data["case_id"]);
    let old =
        f.g.call(&reviewer, "at_resume", json!({"product_id":f.product}))
            .await;
    assert_eq!(old.violations[0]["code"], "STALE_ASSIGNMENT");
    f.write(
        &f.owner,
        "at_accept",
        &other,
        json!({"work_id":other,"reason":"Review recorded"}),
    )
    .await;
}

/// Basic assignment scope and explicit handoff still prevent accidental writes by the wrong agent.
#[tokio::test]
async fn scope_and_transferred_generation_are_enforced() {
    let f = Fixture::new().await;
    let first = f.create(&f.product, "module").await;
    let second = f.create(&f.product, "module").await;
    let lead = f.assign(&first).await;
    let args = f.args(&second, &lead, json!({"work_id":second})).await;
    assert_eq!(
        f.g.call(&lead, "at_begin", args).await.violations[0]["code"],
        "OUT_OF_SCOPE"
    );
    let child = f.create(&first, "task").await;
    let wrong=f.args(&child,&f.owner,json!({"work_id":child,"assignment_id":lead.assignment_id,"new_principal_id":"next","reason":"Wrong target"})).await;
    assert_eq!(
        f.g.call(&f.owner, "at_transfer", wrong).await.violations[0]["code"],
        "OUT_OF_SCOPE"
    );
    let transferred = f
        .write(
            &f.owner,
            "at_transfer",
            &first,
            json!({"work_id":first,"new_principal_id":"next","reason":"Explicit handoff"}),
        )
        .await;
    assert_eq!(transferred.data["new_generation"], 2);
    let old =
        f.g.call(&lead, "at_resume", json!({"product_id":f.product}))
            .await;
    assert_eq!(old.violations[0]["code"], "STALE_ASSIGNMENT");
    let next = Principal {
        id: "next".into(),
        generation: Some(2),
        ..lead
    };
    let resumed =
        f.g.call(&next, "at_resume", json!({"product_id":f.product}))
            .await;
    assert_eq!(resumed.status, "ok");
    f.begin(&first, &next).await;
}

/// Notes and separate publications retain useful versions without requiring content hashes or provenance proofs.
#[tokio::test]
async fn notes_publish_without_hash_ceremony() {
    let f = Fixture::new().await;
    let note = f
        .write(
            &f.owner,
            "at_knowledge_save",
            &f.product,
            json!({"title":"Implementation notes","content":"First version"}),
        )
        .await;
    let first = f
        .write(
            &f.owner,
            "at_knowledge_publish",
            &f.product,
            json!({"note_id":note.data["note_id"]}),
        )
        .await;
    f.write(&f.owner,"at_knowledge_save",&f.product,json!({"note_id":note.data["note_id"],"title":"Implementation notes","content":"Updated version"})).await;
    let second = f
        .write(
            &f.owner,
            "at_knowledge_publish",
            &f.product,
            json!({"note_id":note.data["note_id"]}),
        )
        .await;
    for (publication, expected) in [(&first, "First version"), (&second, "Updated version")] {
        let out=f.g.call(&f.owner,"at_context",json!({"product_id":f.product,"publication_id":publication.data["publication_id"],"section":"publication"})).await;
        let doc: Value = serde_json::from_str(out.data["content"].as_str().unwrap()).unwrap();
        assert_eq!(doc["content"], expected);
    }
}

/// Projection repair is an explicit controller action and exact replay creates no duplicate write.
#[tokio::test]
async fn projection_repair_replays_without_another_write() {
    let f = Fixture::new().await;
    let work = f.create(&f.product, "module").await;
    f.state.lock().await.issues.get_mut(&work).unwrap()["state"] =
        json!({"id":Uuid::new_v4().to_string()});
    let args = f
        .args(
            &work,
            &f.owner,
            json!({"work_id":work,"mode":"restore_projection"}),
        )
        .await;
    let out = f.g.call(&f.owner, "at_reconcile", args.clone()).await;
    assert_eq!(out.status, "committed", "{out:?}");
    let writes = f.state.lock().await.writes;
    assert_eq!(
        f.g.call(&f.owner, "at_reconcile", args).await.status,
        "committed"
    );
    assert_eq!(f.state.lock().await.writes, writes);
}

/// Lost create responses survive a fresh gateway and do not cause duplicate work or changed-payload replay.
#[tokio::test]
async fn lost_response_reconciles_exact_ids_after_restart() {
    let f = Fixture::new().await;
    let args=f.args(&f.product,&f.owner,json!({"kind":"module","parent_id":f.product,"title":"Recovered","description":"once","classification":"research"})).await;
    f.state.lock().await.fail_after = Some("MCreateIssue".into());
    let out = f.g.call(&f.owner, "at_work_create", args.clone()).await;
    assert_eq!(out.status, "outcome_unknown");
    let count = f.state.lock().await.issues.len();
    assert_eq!(count, 2);
    let fresh = Gateway::new(f.g.store.clone()).unwrap();
    let mut inspect = f
        .args(
            &f.product,
            &f.owner,
            json!({"mode":"inspect","operation_key":args["idempotency_key"]}),
        )
        .await;
    let before = f.state.lock().await.writes;
    let out = fresh.call(&f.owner, "at_reconcile", inspect.clone()).await;
    assert_eq!(out.status, "ok");
    assert_eq!(before, f.state.lock().await.writes);
    inspect["mode"] = json!("resume_pending");
    let out = fresh.call(&f.owner, "at_reconcile", inspect).await;
    assert_eq!(out.status, "committed", "{out:?}");
    assert_eq!(f.state.lock().await.issues.len(), count);
    let before = f.state.lock().await.writes;
    let replay = fresh.call(&f.owner, "at_work_create", args.clone()).await;
    assert_eq!(replay.status, "committed");
    assert_eq!(f.state.lock().await.writes, before);
    let mut changed = args;
    changed["title"] = json!("Changed");
    let out = fresh.call(&f.owner, "at_work_create", changed).await;
    assert_eq!(out.violations[0]["code"], "PAYLOAD_MISMATCH");
}

/// A successful write followed by one older signed read triggers no duplicate write.
#[tokio::test]
async fn record_readback_retries_only_the_stale_read() {
    let f = Fixture::new().await;
    let snapshot = f.g.store.snapshot(&f.product).await.unwrap();
    let mut record = snapshot.config.clone();
    let stale = f.state.lock().await.attachments[&record.record_id].clone();
    f.state.lock().await.stale_attachment = Some(stale.clone());
    record.revision += 1;
    record.payload["policy_version"] = json!(2);
    f.g.store.signer.seal(&mut record).unwrap();
    let writes = f.state.lock().await.writes;
    f.g.store
        .put(&record, record.payload["record_base_url"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(f.state.lock().await.writes, writes + 1);
    f.state.lock().await.stale_attachment = Some(stale);
    f.g.store.verify(&record).await.unwrap();
    assert_eq!(f.state.lock().await.writes, writes + 1);
}

/// Invalid shapes and unavailable credentials remain explicit without exposing configured secrets.
#[tokio::test]
async fn schema_roles_and_missing_token_are_enforced() {
    let store = Store {
        linear: Linear::new(None, false).unwrap(),
        signer: Signer::new("secret-signing-material-32-bytes-long").unwrap(),
    };
    let g = Gateway::new(store).unwrap();
    assert_eq!(g.catalog.tools.len(), 28);
    let observer = Principal {
        id: "observer".into(),
        role: Role::Observer,
        products: vec![Uuid::new_v4().to_string()],
        assignment_id: None,
        generation: None,
        epoch: 1,
    };
    assert_eq!(g.catalog.visible(Role::Observer).len(), 3);
    let result = g
        .call(
            &observer,
            "at_resume",
            json!({"product_id":observer.products[0]}),
        )
        .await;
    assert_eq!(result.status, "unavailable");
    assert_eq!(result.violations[0]["code"], "LINEAR_TOKEN_MISSING");
    let result = g.call(&observer, "at_work_create", json!({})).await;
    assert_eq!(result.violations[0]["code"], "UNAUTHORIZED");
    let result = g
        .call(&observer, "at_resume", json!({"product_id":"not-a-uuid"}))
        .await;
    assert_eq!(result.violations[0]["code"], "INVALID_INPUT");
}

/// A stopped writer can finish its exact reserved metadata upsert after the response was lost before application.
#[tokio::test]
async fn pending_head_upsert_resumes_without_operator_record_edit() {
    let f = Fixture::new().await;
    let work = f.create(&f.product, "module").await;
    let args = f
        .args(
            &work,
            &f.owner,
            json!({"work_id":work,"summary":"Recorded progress"}),
        )
        .await;
    f.state.lock().await.drop_head_once = true;
    let interrupted = f.g.call(&f.owner, "at_checkpoint", args.clone()).await;
    assert_eq!(interrupted.status, "outcome_unknown", "{interrupted:?}");
    let fresh = Gateway::new(f.g.store.clone()).unwrap();
    let comments = f.state.lock().await.comments.len();
    let resumed = fresh
        .call(
            &f.owner,
            "at_reconcile",
            json!({"product_id":f.product,"mode":"resume_pending",
        "operation_key":args["idempotency_key"],"idempotency_key":Uuid::new_v4().to_string()}),
        )
        .await;
    assert_eq!(resumed.status, "committed", "{resumed:?}");
    assert_eq!(f.state.lock().await.comments.len(), comments);
    let facts = fresh.store.snapshot(&f.product).await.unwrap();
    assert!(facts.config.payload["pending_operation_key"].is_null());
    assert_eq!(
        facts.work(&work).unwrap().head.payload["activity"]["summary"],
        "Recorded progress"
    );
}
