//! End-to-end gateway checks against an HTTP fixture acting as durable Linear state.

use agent_tasks_linear::{
    gateway::Gateway,
    linear::Linear,
    model::{Outcome, Principal, Role},
    records::{Signer, Store, hash},
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
}
/// Fixture boundary: execute a known static GraphQL operation, never arbitrary GraphQL text.
async fn graphql(State(state): State<Arc<Mutex<Fake>>>, Json(request): Json<Value>) -> Json<Value> {
    let mut s = state.lock().await;
    let op = request["operationName"].as_str().unwrap();
    let v = &request["variables"];
    let input = &v["input"];
    let id = v["id"].as_str().unwrap_or("");
    let data = match op {
        "QIssue" => json!({"issue":s.issues.get(id)}),
        "QAttachmentById" => json!({"attachment":s.attachments.get(id)}),
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
    /// Publish one required criterion with an exact mandatory child manifest.
    async fn plan(&self, work: &str, children: &[&str]) {
        self.write(&self.owner,"at_plan_publish",work,json!({"work_id":work,"goal":"Observe real behavior","scope":"Fixture","criteria":[{"id":"C1","text":"Behavior passes","required":true,"verification":"test"}],"inputs":[],"dependencies":[],"contracts":[],"knowledge_outputs":[],"mandatory_children":children})).await;
    }
    /// Assign a module and construct the corresponding separately authenticated binding.
    async fn assign(&self, work: &str) -> Principal {
        let out=self.write(&self.owner,"at_assign",work,json!({"work_id":work,"principal_id":"lead","role":"lead","scope":"Module and tasks"})).await;
        Principal {
            id: "lead".into(),
            role: Role::Lead,
            products: vec![self.product.clone()],
            assignment_id: out.data["assignment_id"].as_str().map(str::to_owned),
            generation: Some(1),
            epoch: 1,
        }
    }
    /// Record a real-shaped runtime observation and begin work using its exact returned ID.
    async fn begin(&self, work: &str, lead: &Principal) {
        let out=self.write(lead,"at_execution_observe",work,json!({"work_id":work,"assignment_id":lead.assignment_id,"observation":{"runtime":"fixture","run_id":"fixture-run","state":"running","observed_at":"2026-09-24T10:00:00Z","source":{"kind":"document","locator":"fixture://run"},"writer_state":"active"}})).await;
        self.write(lead,"at_begin",work,json!({"work_id":work,"assignment_id":lead.assignment_id,"attempt_id":out.data["attempt_id"]})).await;
    }
}
/// Produce provenance-rich evidence bound to an exact canonical artifact manifest.
fn evidence(artifacts: &Value, result: &str) -> Value {
    json!({"id":Uuid::new_v4().to_string(),"criterion_ids":["C1"],"kind":"test","subject_hash":hash(artifacts).unwrap(),"source_artifacts":artifacts,"command_or_scenario":"fixture integration scenario","environment":"isolated mock Linear HTTP server","observed_at":"2026-09-24T10:00:00Z","result":result,"primary_output":{"kind":"document","locator":"fixture://proof"}})
}

/// A full module remains unaccepted until its child, independent coverage and passed evidence are present.
#[tokio::test]
async fn module_cycle_rejects_failed_review_and_preserves_scopes() {
    let f = Fixture::new().await;
    let module = f.create(&f.product, "module").await;
    let task = f.create(&module, "task").await;
    f.plan(&task, &[]).await;
    f.plan(&module, &[&task]).await;
    let lead = f.assign(&module).await;
    f.begin(&module, &lead).await;
    f.begin(&task, &lead).await;
    let artifacts = json!([{"kind":"document","locator":"fixture://result"}]);
    let task_result=f.write(&lead,"at_task_complete",&task,json!({"work_id":task,"result_summary":"Complete","artifacts":artifacts,"evidence":[evidence(&artifacts,"passed")],"reuse_evidence":[],"knowledge_results":[]})).await;
    assert_eq!(task_result.data["acceptance_level"], "task_local");
    let s = f.g.store.snapshot(&f.product).await.unwrap();
    assert!(s.work(&module).unwrap().head.payload["acceptance_id"].is_null());
    let sub=f.write(&lead,"at_submit",&module,json!({"work_id":module,"summary":"Module result","artifacts":artifacts,"evidence":[evidence(&artifacts,"passed")],"reuse_evidence":[],"knowledge_results":[]})).await;
    let open=f.write(&f.owner,"at_review_open",&module,json!({"work_id":module,"submission_id":sub.data["submission_id"],"reviewer_principal":"reviewer","review_kind":"module","scope":"Full module","required_criteria":["C1"]})).await;
    let reviewer = Principal {
        id: "reviewer".into(),
        role: Role::Reviewer,
        products: vec![f.product.clone()],
        assignment_id: open.data["reviewer_assignment_id"]
            .as_str()
            .map(str::to_owned),
        generation: Some(1),
        epoch: 1,
    };
    let failed = evidence(&artifacts, "failed");
    let report = json!({"work_id":module,"case_id":open.data["case_id"],"submission_id":sub.data["submission_id"],"coverage":[{"criterion_id":"C1","state":"covered","evidence_ids":[failed["id"]]}],"findings":[],"evidence":[failed],"summary":"Must fail"});
    let args = f.args(&module, &reviewer, report).await;
    let before = f.state.lock().await.writes;
    let out = f.g.call(&reviewer, "at_review_report", args).await;
    assert_eq!(out.status, "blocked");
    assert_eq!(f.state.lock().await.writes, before);
    let passed = evidence(&artifacts, "passed");
    f.write(&reviewer,"at_review_report",&module,json!({"work_id":module,"case_id":open.data["case_id"],"submission_id":sub.data["submission_id"],"coverage":[{"criterion_id":"C1","state":"covered","evidence_ids":[passed["id"]]}],"findings":[],"evidence":[passed],"summary":"Independent full review passed"})).await;
    let accepted=f.write(&f.owner,"at_accept",&module,json!({"work_id":module,"submission_id":sub.data["submission_id"],"case_id":open.data["case_id"],"reason":"Evidence checked"})).await;
    assert_eq!(accepted.data["acceptance_level"], "module");
    let outside = f.create(&f.product, "module").await;
    let out =
        f.g.call(
            &lead,
            "at_context",
            json!({"product_id":f.product,"work_id":outside,"section":"work"}),
        )
        .await;
    assert_eq!(out.violations[0]["code"], "OUT_OF_SCOPE");
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

/// Invalid shapes and unavailable credentials remain explicit without exposing configured secrets.
#[tokio::test]
async fn schema_roles_and_missing_token_are_enforced() {
    let store = Store {
        linear: Linear::new(None, false).unwrap(),
        signer: Signer::new("secret-signing-material-32-bytes-long").unwrap(),
    };
    let g = Gateway::new(store).unwrap();
    assert_eq!(g.catalog.tools.len(), 27);
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

/// Human draft edits publish separate snapshots; older pins remain valid until tampered with.
#[tokio::test]
async fn native_drafts_publish_immutable_snapshots() {
    use agent_tasks_linear::records::content_hash;
    let f = Fixture::new().await;
    let note=f.write(&f.owner,"at_knowledge_save",&f.product,json!({"title":"Contract","content":"original content","associations":[],"knowledge_state":"proposed","basis":"reported","basis_refs":[]})).await;
    let first=f.write(&f.owner,"at_knowledge_publish",&f.product,json!({"note_id":note.data["note_id"],"expected_content_hash":content_hash("original content"),"reason":"Publish original","outgoing_publications":[],"basis_refs":[]})).await;
    let draft = note.data["document_id"].as_str().unwrap();
    f.state.lock().await.documents.get_mut(draft).unwrap()["content"] =
        json!("human edited content");
    let args=f.args(&f.product,&f.owner,json!({"note_id":note.data["note_id"],"expected_content_hash":content_hash("original content"),"reason":"Stale request","outgoing_publications":[],"basis_refs":[]})).await;
    let writes = f.state.lock().await.writes;
    let out = f.g.call(&f.owner, "at_knowledge_publish", args).await;
    assert_eq!(out.violations[0]["code"], "DOCUMENT_EDIT_CONFLICT");
    assert_eq!(f.state.lock().await.writes, writes);
    f.write(&f.owner,"at_knowledge_publish",&f.product,json!({"note_id":note.data["note_id"],"expected_content_hash":content_hash("human edited content"),"reason":"Publish human edit","outgoing_publications":[],"basis_refs":[]})).await;
    let out=f.g.call(&f.owner,"at_context",json!({"product_id":f.product,"publication_id":first.data["publication_id"],"section":"publication"})).await;
    assert_eq!(out.status, "ok");
    assert!(
        out.data["content"]
            .as_str()
            .unwrap()
            .contains("original content")
    );
    let s = f.g.store.snapshot(&f.product).await.unwrap();
    let original = s
        .record(
            first.data["publication_id"].as_str().unwrap(),
            Some("publication"),
        )
        .unwrap();
    f.state
        .lock()
        .await
        .documents
        .get_mut(original.payload["document_id"].as_str().unwrap())
        .unwrap()["content"] = json!("tampered");
    let out=f.g.call(&f.owner,"at_context",json!({"product_id":f.product,"publication_id":first.data["publication_id"],"section":"publication"})).await;
    assert_eq!(out.violations[0]["code"], "SNAPSHOT_TAMPERED");
}

/// Transfer rejects active writers, revokes the old generation and requires independent recovery confirmation.
#[tokio::test]
async fn transfer_requires_stop_and_new_generation_recovery() {
    let f = Fixture::new().await;
    let work = f.create(&f.product, "module").await;
    f.plan(&work, &[]).await;
    let lead = f.assign(&work).await;
    f.begin(&work, &lead).await;
    let s = f.g.store.snapshot(&f.product).await.unwrap();
    let assignment = s
        .record(lead.assignment_id.as_deref().unwrap(), Some("assignment"))
        .unwrap();
    let active = assignment.payload["latest_attempt_id"].clone();
    let mut args=f.args(&work,&f.owner,json!({"work_id":work,"assignment_id":lead.assignment_id,"new_principal_id":"next-lead","reason":"Transfer fixture","stop_observation_id":active,"preserved_artifacts":[]})).await;
    args["expected"]["assignment_generation"] = json!(1);
    let out = f.g.call(&f.owner, "at_transfer", args).await;
    assert_eq!(out.violations[0]["code"], "WRITER_ACTIVE");
    let stopped=f.write(&lead,"at_execution_observe",&work,json!({"work_id":work,"assignment_id":lead.assignment_id,"observation":{"runtime":"fixture","run_id":"fixture-run","state":"stopped","observed_at":"2026-09-24T10:01:00Z","source":{"kind":"document","locator":"fixture://stopped"},"writer_state":"stopped"}})).await;
    let mut args=f.args(&work,&f.owner,json!({"work_id":work,"assignment_id":lead.assignment_id,"new_principal_id":"next-lead","reason":"Transfer fixture","stop_observation_id":stopped.data["attempt_id"],"preserved_artifacts":[]})).await;
    args["expected"]["assignment_generation"] = json!(1);
    let transfer = f.g.call(&f.owner, "at_transfer", args).await;
    assert_eq!(transfer.status, "committed", "{transfer:?}");
    let out =
        f.g.call(&lead, "at_resume", json!({"product_id":f.product}))
            .await;
    assert_eq!(out.violations[0]["code"], "STALE_ASSIGNMENT");
    let next = Principal {
        id: "next-lead".into(),
        generation: Some(2),
        ..lead.clone()
    };
    let s = f.g.store.snapshot(&f.product).await.unwrap();
    let plan_hash = s.work(&work).unwrap().head.payload["plan_hash"].clone();
    let args=f.args(&work,&next,json!({"work_id":work,"assignment_id":next.assignment_id,"attempt_id":stopped.data["attempt_id"]})).await;
    let out = f.g.call(&next, "at_begin", args).await;
    assert_eq!(out.violations[0]["code"], "NOT_RECOVERED");
    let report=f.write(&next,"at_recovery_report",&work,json!({"work_id":work,"assignment_id":next.assignment_id,"understood_plan_hash":plan_hash,"examined_artifacts":[],"outstanding":"None","risks":"None","proposed_next_step":"Continue"})).await;
    let mut args=f.args(&work,&f.owner,json!({"work_id":work,"assignment_id":next.assignment_id,"recovery_report_id":report.data["recovery_report_id"],"reason":"Reviewed recovery"})).await;
    args["expected"]["assignment_generation"] = json!(2);
    let out = f.g.call(&f.owner, "at_recovery_confirm", args).await;
    assert_eq!(out.status, "committed", "{out:?}");
    f.begin(&work, &next).await;
}

/// Frozen scope can grow only through an approved proposal followed by an exact new plan.
#[tokio::test]
async fn approved_scope_change_can_add_a_mandatory_child() {
    let f = Fixture::new().await;
    let work = f.create(&f.product, "module").await;
    f.plan(&work, &[]).await;
    let s = f.g.store.snapshot(&f.product).await.unwrap();
    let base = s.work(&work).unwrap().head.payload["plan_hash"].clone();
    let proposal=f.write(&f.owner,"at_change_propose",&work,json!({"work_id":work,"base_plan_hash":base,"reason":"Add test","proposed_change":"Add one task","affected_work_ids":[work],"risk":"None"})).await;
    let decision=f.write(&f.owner,"at_owner_decide",&work,json!({"record_id":proposal.data["proposal_id"],"decision_key":"approve","rationale":"Proceed","source_kind":"owner_authenticated"})).await;
    let task = f.create(&work, "task").await;
    f.write(&f.owner,"at_plan_publish",&work,json!({"work_id":work,"goal":"Updated result","scope":"One task","criteria":[{"id":"C1","text":"Behavior passes","required":true,"verification":"test"}],"inputs":[],"dependencies":[],"contracts":[],"knowledge_outputs":[],"mandatory_children":[task],"change_proposal_id":proposal.data["proposal_id"],"owner_decision_id":decision.data["decision_id"]})).await;
    let s = f.g.store.snapshot(&f.product).await.unwrap();
    assert_eq!(
        s.work(&work).unwrap().head.payload["pending_scope_change"],
        false
    );
}
