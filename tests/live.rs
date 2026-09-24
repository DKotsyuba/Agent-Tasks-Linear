//! Explicitly opted-in acceptance against an isolated real Linear product; never runs by default.

use agent_tasks_linear::{
    config::Config,
    gateway::Gateway,
    linear::Linear,
    model::{Outcome, Principal, Role},
    records::{Signer, Store, hash},
    server,
};
use rmcp::{
    ServiceExt,
    model::CallToolRequestParams,
    transport::{
        StreamableHttpClientTransport, TokioChildProcess,
        streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Live fixture holding only deployment secrets; all workflow facts remain in Linear.
struct Live {
    /// Shared production gateway; replaced during the cold-start check.
    gateway: Arc<Gateway>,
    /// Protected deployment configuration read from an explicitly supplied path.
    config: Config,
    /// Private provisioned role bindings for this run, created using production configuration code.
    binding_path: PathBuf,
    /// Product control UUID supplied after reviewed bootstrap.
    product: String,
    /// Safe evidence output path, excluded from the repository.
    report_path: PathBuf,
    /// Tool inputs/outcomes; contains no authentication tokens or signing keys.
    report: Value,
    /// Historical calls eligible for exact replay; fresh calls never accidentally reuse an earlier stage.
    resume_calls: usize,
}
impl Live {
    /// Connect only when all three live-test environment variables were deliberately supplied.
    fn new() -> Self {
        let path = std::env::var("ATL_LIVE_CONFIG").expect("ATL_LIVE_CONFIG is required");
        let config = Config::load(std::path::Path::new(&path)).unwrap();
        let product = std::env::var("ATL_LIVE_PRODUCT").expect("ATL_LIVE_PRODUCT is required");
        Uuid::parse_str(&product).unwrap();
        let token = std::env::var("LINEAR_API_KEY").expect("LINEAR_API_KEY is required");
        let gateway = Gateway::new(Store {
            linear: Linear::new(Some(token), false).unwrap(),
            signer: Signer::new(&config.signing_key).unwrap(),
        })
        .unwrap();
        std::fs::create_dir_all(".local").unwrap();
        let report_path = std::env::var_os("ATL_LIVE_REPORT")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .unwrap()
                    .join(format!(".local/live-{}.json", Uuid::new_v4()))
            });
        let mut report: Value = if report_path.exists() {
            serde_json::from_slice(&std::fs::read(&report_path).unwrap()).unwrap()
        } else {
            json!({"product_id":product,"started_at":agent_tasks_linear::model::now(),"calls":[],"complete":false})
        };
        assert_eq!(
            report["product_id"], product,
            "Report belongs to another product"
        );
        let run = report["run_id"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| {
                report["calls"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| row["tool"] == "at_knowledge_save")
                    .and_then(|row| row["arguments"]["title"].as_str())
                    .and_then(|title| title.strip_prefix("Live check "))
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        report["run_id"] = json!(run);
        let binding_path = report_path.with_extension("toml");
        config.write_new(&binding_path).unwrap();
        let resume_calls = report["calls"].as_array().unwrap().len();
        println!("Live report: {}", report_path.display());
        Self {
            gateway,
            config,
            binding_path,
            product,
            report_path,
            report,
            resume_calls,
        }
    }
    /// Resolve the locally provisioned owner without exposing its secret to test output.
    fn owner(&self) -> Principal {
        self.config
            .bindings
            .iter()
            .find(|b| b.principal.role == Role::Owner)
            .unwrap()
            .principal
            .clone()
    }
    /// Provision an assignment with the same protected-file mechanism used by the administrative CLI.
    fn bind(&mut self, name: &str, principal: Principal) {
        Config::add_binding(&self.binding_path, name.into(), principal).unwrap();
        self.config = Config::load(&self.binding_path).unwrap();
    }
    /// Use the real authenticated HTTP MCP surface for one identity and preserve its observable result.
    async fn call(&mut self, principal: &Principal, name: &str, args: Value) -> Outcome {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let mut config = Config::load(&self.binding_path).unwrap();
        let binding = config
            .bindings
            .iter()
            .find(|b| b.principal.id == principal.id)
            .expect("principal was not provisioned")
            .clone();
        assert!(
            serde_json::to_value(&binding.principal).unwrap()
                == serde_json::to_value(principal).unwrap(),
            "binding generation or scope does not match"
        );
        config.listen = address;
        let cancel = CancellationToken::new();
        let router = server::router(self.gateway.clone(), &config, cancel.clone());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let transport = StreamableHttpClientTransport::with_client(
            reqwest::Client::new(),
            StreamableHttpClientTransportConfig::with_uri(format!(
                "http://{address}/mcp/{}",
                binding.name
            ))
            .auth_header(binding.token),
        );
        let client = ().serve(transport).await.unwrap();
        let reply = client
            .peer()
            .call_tool(
                CallToolRequestParams::new(name.to_owned())
                    .with_arguments(args.as_object().unwrap().clone()),
            )
            .await
            .unwrap();
        let wire = serde_json::to_value(reply).unwrap();
        let outcome: Outcome = serde_json::from_value(wire["structuredContent"].clone()).unwrap();
        client.cancel().await.unwrap();
        cancel.cancel();
        task.abort();
        self.report["calls"].as_array_mut().unwrap().push(json!({"principal_id":principal.id,"role":principal.role,"tool":name,"arguments":args,"outcome":outcome}));
        std::fs::write(
            &self.report_path,
            serde_json::to_vec_pretty(&self.report).unwrap(),
        )
        .unwrap();
        println!("{name}: {}", outcome.status);
        outcome
    }
    /// Populate current expected tokens from authoritative facts before exercising a write boundary.
    async fn mutate(
        &mut self,
        principal: &Principal,
        name: &str,
        work: &str,
        args: Value,
    ) -> Outcome {
        let previous = self.report["calls"]
            .as_array()
            .unwrap()
            .iter()
            .take(self.resume_calls)
            .find(|row| {
                let a = &row["arguments"];
                let target = a["work_id"]
                    .as_str()
                    .or(a["parent_id"].as_str())
                    .or(a["epic_id"].as_str())
                    .or(a["product_id"].as_str());
                row["principal_id"] == principal.id
                    && row["tool"] == name
                    && (target == Some(work)
                        || (a["record_id"].is_string() && a["record_id"] == args["record_id"]))
                    && matches!(
                        row["outcome"]["status"].as_str(),
                        Some("committed" | "noop" | "outcome_unknown")
                    )
                    && [
                        "title",
                        "kind",
                        "summary",
                        "change_proposal_id",
                        "expected_content_hash",
                        "record_id",
                    ]
                    .iter()
                    .all(|field| a[*field] == args[*field])
                    && a["observation"]["state"] == args["observation"]["state"]
            })
            .map(|row| row["arguments"].clone());
        if let Some(original) = previous {
            let result = self.call(principal, name, original).await;
            assert!(
                matches!(result.status.as_str(), "committed" | "noop"),
                "Saved intent must reconcile before this test continues: {result:?}"
            );
            return result;
        }
        let args = self.arguments(principal, work, args).await;
        let result = self.call(principal, name, args).await;
        assert!(
            matches!(result.status.as_str(), "committed" | "noop"),
            "{name} failed: {result:?}"
        );
        result
    }
    /// Fill current revision/plan/generation tokens for either a positive or negative live boundary check.
    async fn arguments(&self, principal: &Principal, work: &str, mut args: Value) -> Value {
        let facts = self.gateway.store.snapshot(&self.product).await.unwrap();
        let head = &facts.work(work).unwrap().head;
        args["product_id"] = json!(self.product);
        args["idempotency_key"] = json!(Uuid::new_v4().to_string());
        args["expected"] = json!({"work_revision":head.revision});
        if head.payload["plan_hash"].is_string() {
            args["expected"]["plan_hash"] = head.payload["plan_hash"].clone();
        }
        if let Some(generation) = principal.generation {
            args["expected"]["assignment_generation"] = json!(generation);
        } else if let Some(id) = args["assignment_id"].as_str() {
            args["expected"]["assignment_generation"] =
                facts.record(id, Some("assignment")).unwrap().payload["generation"].clone();
        }
        args
    }
    /// Verify a stable rejection; resumed runs retain a matching earlier negative assertion.
    async fn rejects(
        &mut self,
        principal: &Principal,
        name: &str,
        work: &str,
        args: Value,
        code: &str,
    ) {
        if self.report["calls"]
            .as_array()
            .unwrap()
            .iter()
            .take(self.resume_calls)
            .any(|r| {
                r["tool"] == name
                    && r["arguments"]["work_id"] == work
                    && r["outcome"]["violations"][0]["code"] == code
            })
        {
            return;
        }
        let args = self.arguments(principal, work, args).await;
        let result = self.call(principal, name, args).await;
        assert_eq!(result.violations[0]["code"], code, "{name}: {result:?}");
    }
    /// Publish a small observable workflow test plan with exact required children.
    async fn plan(&mut self, owner: &Principal, work: &str, children: &[String]) {
        self.mutate(owner,"at_plan_publish",work,json!({"work_id":work,"goal":"Verify the real authenticated workflow","scope":"Isolated Linear integration test","criteria":[{"id":"C1","text":"Authenticated work context and pinned output can be read","required":true,"verification":"test"}],"inputs":[],"dependencies":[],"contracts":[],"knowledge_outputs":[],"mandatory_children":children})).await;
    }
    /// Record the observed execution of this actual test process, then begin the permitted work.
    async fn begin(&mut self, lead: &Principal, work: &str) {
        let run = format!("live-test-{}", std::process::id());
        let observation=self.mutate(lead,"at_execution_observe",work,json!({"work_id":work,"assignment_id":lead.assignment_id,"observation":{"runtime":"cargo-test","run_id":run,"state":"running","observed_at":agent_tasks_linear::model::now(),"source":{"kind":"file","locator":self.report_path},"writer_state":"active"}})).await;
        self.mutate(lead,"at_begin",work,json!({"work_id":work,"assignment_id":lead.assignment_id,"attempt_id":observation.data["attempt_id"]})).await;
    }
    /// Describe a read assertion already performed by this process and its persisted primary output.
    fn evidence(&self, artifacts: &Value) -> Value {
        json!({"id":Uuid::new_v4().to_string(),"criterion_ids":["C1"],"kind":"test","subject_hash":hash(artifacts).unwrap(),"source_artifacts":artifacts,"command_or_scenario":"Live authenticated MCP assertions for exact work IDs, parent membership and pinned content hashes; see primary output","environment":"Real isolated Linear workspace through authenticated HTTP MCP","observed_at":agent_tasks_linear::model::now(),"result":"passed","primary_output":{"kind":"file","locator":self.report_path}})
    }
    /// Start a new executable in an empty working directory and read accepted state through its stdio bridge.
    async fn cold_resume(&self, work: &str) -> Outcome {
        let directory = std::env::current_dir()
            .unwrap()
            .join(format!(".local/cold-runtime-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("runtime.toml");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let mut config = Config::load(&self.binding_path).unwrap();
        config.listen = address;
        config.write_new(&path).unwrap();
        let mut process = tokio::process::Command::new(env!("CARGO_BIN_EXE_agent-tasks-linear"))
            .arg("--config")
            .arg(&path)
            .arg("serve")
            .current_dir(&directory)
            .stdout(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let http = reqwest::Client::new();
        let mut ready = false;
        for _ in 0..50 {
            assert!(
                process.try_wait().unwrap().is_none(),
                "fresh gateway exited during startup"
            );
            if http
                .get(format!("http://{address}/health"))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                ready = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(ready, "fresh gateway did not become ready");
        let owner_name = config
            .bindings
            .iter()
            .find(|b| b.principal.role == Role::Owner)
            .unwrap()
            .name
            .clone();
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_agent-tasks-linear"));
        command
            .arg("--config")
            .arg(&path)
            .args(["stdio", "--binding", &owner_name])
            .current_dir(&directory)
            .kill_on_drop(true);
        let bridge = ().serve(TokioChildProcess::new(command).unwrap()).await.unwrap();
        let result = bridge
            .peer()
            .call_tool(
                CallToolRequestParams::new("at_resume").with_arguments(
                    json!({"product_id":self.product,"work_id":work})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .unwrap();
        let wire = serde_json::to_value(result).unwrap();
        let outcome = serde_json::from_value(wire["structuredContent"].clone()).unwrap();
        bridge.cancel().await.unwrap();
        process.kill().await.unwrap();
        let _ = process.wait().await;
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
        outcome
    }
}

impl Drop for Live {
    /// Remove only this run's private binding copy; preserve the non-secret evidence report.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.binding_path);
    }
}

/// Inspect and resume the original saved intent after a diagnosed transport or consistency failure.
#[tokio::test]
#[ignore = "Requires explicit isolated Linear product with an already inspected pending operation"]
async fn reconcile_pending_live_operation() {
    let mut live = Live::new();
    let owner = live.owner();
    let snapshot = live.gateway.store.snapshot(&live.product).await.unwrap();
    let key = snapshot.config.payload["pending_operation_key"]
        .as_str()
        .expect("no pending operation");
    let mut args = json!({"product_id":live.product,"idempotency_key":Uuid::new_v4().to_string(),"expected":{"work_revision":snapshot.work(&live.product).unwrap().head.revision},"mode":"inspect","operation_key":key});
    let inspected = live.call(&owner, "at_reconcile", args.clone()).await;
    assert_eq!(inspected.status, "ok", "{inspected:?}");
    args["mode"] = json!("resume_pending");
    let result = live.call(&owner, "at_reconcile", args).await;
    assert!(
        matches!(result.status.as_str(), "committed" | "noop"),
        "{result:?}"
    );
}

/// Explicit disposable-workspace intervention for an unchanged head marker left unconfirmed after a stopped test process.
#[tokio::test]
#[ignore = "Requires ATL_LIVE_ALLOW_RECORD_REPAIR=1 and explicit owner authorization for the disposable workspace"]
async fn complete_confirmed_test_head_marker() {
    assert_eq!(
        std::env::var("ATL_LIVE_ALLOW_RECORD_REPAIR").as_deref(),
        Ok("1")
    );
    let mut live = Live::new();
    let owner = live.owner();
    let facts = live.gateway.store.snapshot(&live.product).await.unwrap();
    let key = facts.config.payload["pending_operation_key"]
        .as_str()
        .expect("no pending intent");
    let receipt = facts
        .records
        .values()
        .find(|r| r.record_kind == "operation_receipt" && r.payload["idempotency_key"] == key)
        .unwrap();
    let steps = receipt.payload["steps"].as_array().unwrap();
    let started = steps
        .iter()
        .enumerate()
        .filter(|(_, s)| *s == "started")
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    assert_eq!(started.len(), 1);
    let effect = &receipt.payload["plan"]["effects"][started[0]];
    assert_eq!(effect["kind"], "record");
    let expected: agent_tasks_linear::model::Record =
        serde_json::from_value(effect["record"].clone()).unwrap();
    assert_eq!(expected.record_kind, "work_head");
    live.gateway.store.signer.verify(&expected).unwrap();
    let current = facts
        .record(&expected.record_id, Some("work_head"))
        .unwrap();
    assert_eq!(expected.revision, current.revision + 1);
    assert_eq!(expected.payload["last_committed_operation"], key);
    let mut before = current.payload.clone();
    let mut after = expected.payload.clone();
    before
        .as_object_mut()
        .unwrap()
        .remove("last_committed_operation");
    after
        .as_object_mut()
        .unwrap()
        .remove("last_committed_operation");
    assert_eq!(
        before, after,
        "Operator repair must not alter workflow fields"
    );
    live.gateway
        .store
        .put(&expected, effect["base_url"].as_str().unwrap())
        .await
        .unwrap();
    live.report["operator_record_repair"] = json!({"operation_key":key,"record_id":expected.record_id,"from_revision":current.revision,"to_revision":expected.revision,"scope":"Only the last-operation head marker; workflow fields unchanged"});
    let result=live.call(&owner,"at_reconcile",json!({"product_id":live.product,"idempotency_key":Uuid::new_v4().to_string(),"expected":{"work_revision":facts.work(&live.product).unwrap().head.revision},"mode":"resume_pending","operation_key":key})).await;
    assert_eq!(result.status, "committed", "{result:?}");
}

/// Exercise live snapshots, hierarchy, role separation, local completion, review, acceptance and cold restart.
#[tokio::test]
#[ignore = "Requires explicit isolated Linear product, private configuration and API key"]
async fn live_workspace_workflow() {
    let mut live = Live::new();
    let owner = live.owner();
    let product = live.product.clone();
    let run = live.report["run_id"].as_str().unwrap().to_owned();
    let resume = live
        .call(&owner, "at_resume", json!({"product_id":product}))
        .await;
    assert_eq!(resume.status, "ok", "{resume:?}");
    assert!(
        resume.data["pending_operation_key"].is_null(),
        "Reconcile the prior attempt before another run"
    );
    let note=live.mutate(&owner,"at_knowledge_save",&product,json!({"title":format!("Live check {run}"),"content":"A live API round-trip verified by the Rust MCP test.","associations":[],"knowledge_state":"proposed","basis":"reported","basis_refs":[]})).await;
    let publication=live.mutate(&owner,"at_knowledge_publish",&product,json!({"note_id":note.data["note_id"],"expected_content_hash":note.data["content_hash"],"reason":"Exercise a real immutable snapshot","outgoing_publications":[],"basis_refs":[]})).await;
    let read=live.call(&owner,"at_context",json!({"product_id":product,"publication_id":publication.data["publication_id"],"section":"publication"})).await;
    assert_eq!(read.status, "ok");
    let document: Value = serde_json::from_str(read.data["content"].as_str().unwrap()).unwrap();
    assert_eq!(
        document["content"],
        "A live API round-trip verified by the Rust MCP test."
    );
    assert_eq!(
        agent_tasks_linear::records::content_hash(document["content"].as_str().unwrap()),
        publication.data["content_hash"].as_str().unwrap()
    );
    let facts = live.gateway.store.snapshot(&product).await.unwrap();
    let general = facts.config.payload["general_project_id"].clone();
    assert_eq!(document["project"]["id"], general);
    let artifacts = json!([{"kind":"document","locator":document["url"],"sha256":publication.data["content_hash"]}]);
    let module=live.mutate(&owner,"at_work_create",&product,json!({"kind":"module","parent_id":product,"title":format!("Live module {run}"),"description":"Synthetic integration acceptance; no production work.","classification":"research"})).await.data["created_work_id"].as_str().unwrap().to_owned();
    let task=live.mutate(&owner,"at_work_create",&module,json!({"kind":"task","parent_id":module,"title":"Verify authenticated context","description":"Read the real work and document snapshot.","classification":"research"})).await.data["created_work_id"].as_str().unwrap().to_owned();
    live.plan(&owner, &task, &[]).await;
    live.plan(&owner, &module, std::slice::from_ref(&task))
        .await;
    let assigned=live.mutate(&owner,"at_assign",&module,json!({"work_id":module,"principal_id":format!("live-lead-{run}"),"role":"lead","scope":"This synthetic module and task"})).await;
    let lead = Principal {
        id: format!("live-lead-{run}"),
        role: Role::Lead,
        products: vec![product.clone()],
        assignment_id: assigned.data["assignment_id"].as_str().map(str::to_owned),
        generation: Some(1),
        epoch: 1,
    };
    live.bind("lead", lead.clone());
    live.begin(&lead, &module).await;
    live.begin(&lead, &task).await;
    let read = live
        .call(
            &lead,
            "at_context",
            json!({"product_id":product,"work_id":task,"section":"work"}),
        )
        .await;
    assert_eq!(read.status, "ok");
    let context: Value = serde_json::from_str(read.data["content"].as_str().unwrap()).unwrap();
    assert_eq!(context["native"]["id"], task);
    assert_eq!(context["native"]["parent"]["id"], module);
    assert_eq!(context["native"]["project"]["id"], general);
    assert_eq!(context["head"]["plan_hash"], read.version["plan_hash"]);
    let outside = live
        .call(
            &lead,
            "at_context",
            json!({"product_id":product,"work_id":product,"section":"work"}),
        )
        .await;
    assert_eq!(outside.violations[0]["code"], "OUT_OF_SCOPE");
    let evidence = live.evidence(&artifacts);
    live.mutate(&lead,"at_task_complete",&task,json!({"work_id":task,"result_summary":"Real authenticated context was read successfully","artifacts":artifacts,"evidence":[evidence],"reuse_evidence":[],"knowledge_results":[]})).await;
    let evidence = live.evidence(&artifacts);
    let submitted=live.mutate(&lead,"at_submit",&module,json!({"work_id":module,"summary":"Live workflow module completed","artifacts":artifacts,"evidence":[evidence],"reuse_evidence":[],"knowledge_results":[]})).await;
    if !live.report["calls"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["outcome"]["violations"][0]["code"] == "NOT_INDEPENDENT")
    {
        let facts = live.gateway.store.snapshot(&product).await.unwrap();
        let head = &facts.work(&module).unwrap().head;
        let invalid=live.call(&owner,"at_review_open",json!({"product_id":product,"idempotency_key":Uuid::new_v4().to_string(),"expected":{"work_revision":head.revision,"plan_hash":head.payload["plan_hash"]},"work_id":module,"submission_id":submitted.data["submission_id"],"reviewer_principal":lead.id,"review_kind":"module","scope":"Must reject author review","required_criteria":["C1"]})).await;
        assert_eq!(invalid.violations[0]["code"], "NOT_INDEPENDENT");
    }
    let review=live.mutate(&owner,"at_review_open",&module,json!({"work_id":module,"submission_id":submitted.data["submission_id"],"reviewer_principal":format!("live-reviewer-{run}"),"review_kind":"module","scope":"Synthetic workflow and document integrity","required_criteria":["C1"]})).await;
    let reviewer = Principal {
        id: format!("live-reviewer-{run}"),
        role: Role::Reviewer,
        products: vec![product.clone()],
        assignment_id: review.data["reviewer_assignment_id"]
            .as_str()
            .map(str::to_owned),
        generation: Some(1),
        epoch: 1,
    };
    live.bind("reviewer", reviewer.clone());
    let read = live
        .call(
            &reviewer,
            "at_context",
            json!({"product_id":product,"work_id":module,"section":"work"}),
        )
        .await;
    assert_eq!(read.status, "ok");
    let context: Value = serde_json::from_str(read.data["content"].as_str().unwrap()).unwrap();
    assert_eq!(context["native"]["id"], module);
    assert_eq!(
        context["head"]["current_submission_id"],
        submitted.data["submission_id"]
    );
    let frozen=live.call(&reviewer,"at_context",json!({"product_id":product,"record_id":submitted.data["submission_id"],"section":"record"})).await;
    assert_eq!(frozen.status, "ok");
    let frozen: Value = serde_json::from_str(frozen.data["content"].as_str().unwrap()).unwrap();
    assert_eq!(frozen["payload"]["artifacts"], artifacts);
    assert_eq!(frozen["payload"]["subject_hash"], hash(&artifacts).unwrap());
    assert_eq!(
        frozen["payload"]["child_result_ids"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let evidence = live.evidence(&artifacts);
    live.mutate(&reviewer,"at_review_report",&module,json!({"work_id":module,"case_id":review.data["case_id"],"submission_id":submitted.data["submission_id"],"coverage":[{"criterion_id":"C1","state":"covered","evidence_ids":[evidence["id"]]}],"findings":[],"evidence":[evidence],"summary":"Separate authenticated test identity verified current context"})).await;
    let accepted=live.mutate(&owner,"at_accept",&module,json!({"work_id":module,"submission_id":submitted.data["submission_id"],"case_id":review.data["case_id"],"reason":"Synthetic live workflow assertions passed"})).await;
    assert_eq!(accepted.data["acceptance_level"], "module");
    let original = live.report["calls"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["tool"] == "at_work_create")
        .unwrap()["arguments"]
        .clone();
    let replay = live.call(&owner, "at_work_create", original.clone()).await;
    assert_eq!(replay.status, "committed");
    assert_eq!(replay.data["created_work_id"], module);
    let mut changed = original;
    changed["title"] = json!("Changed replay must fail");
    let mismatch = live.call(&owner, "at_work_create", changed).await;
    assert_eq!(mismatch.violations[0]["code"], "PAYLOAD_MISMATCH");
    let resumed = live.cold_resume(&module).await;
    assert_eq!(resumed.status, "ok");
    assert!(
        resumed.data["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["work_id"] == module && w["state"] == "accepted")
    );
    live.report["complete"] = json!(true);
    live.report["module_id"] = json!(module);
    live.report["task_id"] = json!(task);
    live.report["acceptance_id"] = accepted.data["acceptance_id"].clone();
    live.report["cold_restart"] = json!({"new_gateway_process":true,"empty_working_directory":true,"stdio_bridge":true,"outcome":resumed});
    live.report["review_scope"] = json!(
        "Separate provisioned test identities in a synthetic workflow; not independent human or agent review"
    );
    std::fs::write(
        &live.report_path,
        serde_json::to_vec_pretty(&live.report).unwrap(),
    )
    .unwrap();
    println!("Live module accepted: {module}");
}

/// Exercise contract gates, owner attribution, transfer and post-acceptance observations on real Linear.
#[tokio::test]
#[ignore = "Requires explicit isolated Linear product and a completed basic live pilot"]
async fn live_extended_gates() {
    let mut live = Live::new();
    let owner = live.owner();
    let product = live.product.clone();
    let run = live.report["run_id"].as_str().unwrap().to_owned();
    let facts = live.gateway.store.snapshot(&product).await.unwrap();
    assert!(facts.config.payload["pending_operation_key"].is_null());
    let original = live.report["original_module_id"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| {
            facts
                .works
                .iter()
                .find(|(_, w)| {
                    w.identity.payload["kind"] == "module" && w.head.payload["state"] == "accepted"
                })
                .unwrap()
                .0
                .clone()
        });
    live.report["original_module_id"] = json!(original);
    let read = live
        .call(
            &owner,
            "at_context",
            json!({"product_id":product,"work_id":original,"section":"work"}),
        )
        .await;
    assert_eq!(read.status, "ok");
    let context: Value = serde_json::from_str(read.data["content"].as_str().unwrap()).unwrap();
    assert_eq!(context["head"]["state"], "accepted");
    let accepted_before = context["head"]["acceptance_id"].clone();
    let assignment = facts
        .record(
            facts.work(&original).unwrap().head.payload["assignment_ids"][0]
                .as_str()
                .unwrap(),
            Some("assignment"),
        )
        .unwrap();
    let prior_attempt = facts
        .record(
            assignment.payload["latest_attempt_id"].as_str().unwrap(),
            Some("attempt"),
        )
        .unwrap();
    let finished = Principal {
        id: assignment.payload["principal_id"].as_str().unwrap().into(),
        role: Role::Lead,
        products: vec![product.clone()],
        assignment_id: Some(assignment.record_id.clone()),
        generation: assignment.payload["generation"].as_u64(),
        epoch: 1,
    };
    live.bind("finished-lead", finished.clone());
    live.mutate(&finished,"at_execution_observe",&original,json!({"work_id":original,"assignment_id":finished.assignment_id,"observation":{"runtime":prior_attempt.payload["observation"]["runtime"],"run_id":prior_attempt.payload["observation"]["run_id"],"state":"succeeded","observed_at":agent_tasks_linear::model::now(),"source":{"kind":"file","locator":live.report_path},"writer_state":"stopped"}})).await;
    let fresh = live.gateway.store.snapshot(&product).await.unwrap();
    assert_eq!(
        fresh.work(&original).unwrap().head.payload["acceptance_id"],
        accepted_before
    );
    assert_eq!(
        fresh.work(&original).unwrap().head.payload["state"],
        "accepted"
    );
    let search = live
        .call(
            &owner,
            "at_search",
            json!({"product_id":product,"query":"Live module","kind":"both","limit":30}),
        )
        .await;
    assert_eq!(search.status, "ok", "{search:?}");
    assert!(
        search.data["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == original)
    );
    let publication = facts
        .records
        .values()
        .find(|r| r.record_kind == "publication")
        .unwrap()
        .clone();
    let source = json!({"kind":"linear_publication","id":publication.record_id,"sha256":publication.payload["content_hash"]});
    let module=live.mutate(&owner,"at_work_create",&product,json!({"kind":"module","parent_id":product,"title":format!("Live contract gates {run}"),"description":"Synthetic contract, question and recovery checks","classification":"research"})).await.data["created_work_id"].as_str().unwrap().to_owned();
    live.mutate(&owner,"at_plan_publish",&module,json!({"work_id":module,"goal":"Verify contract and owner decision gates","scope":"Synthetic module","criteria":[{"id":"C1","text":"Pinned contract can be read","required":true,"verification":"test"}],"inputs":[],"dependencies":[],"contracts":[{"source":source,"direction":"provides","parties":[module],"preparation_required":true,"real_integration_required":false}],"knowledge_outputs":[],"mandatory_children":[]})).await;
    let assigned=live.mutate(&owner,"at_assign",&module,json!({"work_id":module,"principal_id":format!("contract-lead-{run}"),"role":"lead","scope":"Synthetic contract module"})).await;
    let lead = Principal {
        id: format!("contract-lead-{run}"),
        role: Role::Lead,
        products: vec![product.clone()],
        assignment_id: assigned.data["assignment_id"].as_str().map(str::to_owned),
        generation: Some(1),
        epoch: 1,
    };
    live.bind("contract-lead", lead.clone());
    let pinned=live.call(&lead,"at_context",json!({"product_id":product,"publication_id":publication.record_id,"section":"publication"})).await;
    assert_eq!(pinned.status, "ok");
    let document: Value = serde_json::from_str(pinned.data["content"].as_str().unwrap()).unwrap();
    assert_eq!(
        agent_tasks_linear::records::content_hash(document["content"].as_str().unwrap()),
        publication.payload["content_hash"]
    );
    let artifact = json!({"kind":"document","locator":document["url"],"sha256":publication.payload["content_hash"]});
    let observed=live.mutate(&lead,"at_execution_observe",&module,json!({"work_id":module,"assignment_id":lead.assignment_id,"observation":{"runtime":"cargo-test-phase","run_id":format!("contract-phase-{run}"),"state":"running","observed_at":agent_tasks_linear::model::now(),"source":{"kind":"file","locator":live.report_path},"writer_state":"active"}})).await;
    live.rejects(&lead,"at_begin",&module,json!({"work_id":module,"assignment_id":lead.assignment_id,"attempt_id":observed.data["attempt_id"]}),"CONTRACT_UNAGREED").await;
    live.mutate(&lead,"at_contract_confirm",&module,json!({"work_id":module,"contract_source":source,"agreement":"agree","preparation":"ready","statement":"Exact publication content and hash were read through MCP","observation":artifact})).await;
    live.mutate(&lead,"at_begin",&module,json!({"work_id":module,"assignment_id":lead.assignment_id,"attempt_id":observed.data["attempt_id"]})).await;
    let question=live.mutate(&lead,"at_question_ask",&module,json!({"work_id":module,"question":"Continue this synthetic live test?","options":[{"key":"approve","text":"Continue the test"},{"key":"hold","text":"Hold the test"}],"recommended_key":"approve","blocked_work_ids":[module],"reason":"Exercise explicit owner approval"})).await;
    live.rejects(&lead,"at_begin",&module,json!({"work_id":module,"assignment_id":lead.assignment_id,"attempt_id":observed.data["attempt_id"]}),"OWNER_DECISION_REQUIRED").await;
    let comment_id = Uuid::new_v4().to_string();
    let comment=live.gateway.store.linear.call("MCreateComment",json!({"input":{"id":comment_id,"issueId":module,"body":format!("{} approve",question.data["decision_token"].as_str().unwrap())}})).await.unwrap();
    assert!(
        facts.config.payload["owner_linear_user_ids"]
            .as_array()
            .unwrap()
            .contains(&comment["commentCreate"]["comment"]["user"]["id"])
    );
    live.rejects(&owner,"at_owner_decide",&module,json!({"record_id":question.data["question_id"],"decision_key":"approve","rationale":"PAT comment must not impersonate a human decision","source_kind":"linear_comment","source_comment_id":comment_id}),"UNTRUSTED_OWNER_SOURCE").await;
    live.mutate(&owner,"at_owner_decide",&module,json!({"record_id":question.data["question_id"],"decision_key":"approve","rationale":"Authenticated owner authorizes the synthetic test","source_kind":"owner_authenticated"})).await;
    live.mutate(&lead,"at_checkpoint",&module,json!({"work_id":module,"summary":"Contract read and owner attribution checks passed","remaining":"Transfer responsibility and retire this test work","artifacts":[artifact]})).await;
    let stopped=live.mutate(&lead,"at_execution_observe",&module,json!({"work_id":module,"assignment_id":lead.assignment_id,"observation":{"runtime":"cargo-test-phase","run_id":format!("contract-phase-{run}"),"state":"suspended","observed_at":agent_tasks_linear::model::now(),"source":{"kind":"file","locator":live.report_path},"writer_state":"stopped"}})).await;
    live.mutate(&owner,"at_transfer",&module,json!({"work_id":module,"assignment_id":lead.assignment_id,"new_principal_id":format!("next-lead-{run}"),"reason":"Synthetic recovery test","stop_observation_id":stopped.data["attempt_id"],"preserved_artifacts":[artifact]})).await;
    let denied = live
        .call(
            &lead,
            "at_resume",
            json!({"product_id":product,"work_id":module}),
        )
        .await;
    assert_eq!(denied.violations[0]["code"], "STALE_ASSIGNMENT");
    let next = Principal {
        id: format!("next-lead-{run}"),
        generation: Some(2),
        ..lead.clone()
    };
    live.bind("next-lead", next.clone());
    live.rejects(&next,"at_begin",&module,json!({"work_id":module,"assignment_id":next.assignment_id,"attempt_id":stopped.data["attempt_id"]}),"NOT_RECOVERED").await;
    let current = live.gateway.store.snapshot(&product).await.unwrap();
    let plan_hash = current.work(&module).unwrap().head.payload["plan_hash"].clone();
    let recovery=live.mutate(&next,"at_recovery_report",&module,json!({"work_id":module,"assignment_id":next.assignment_id,"understood_plan_hash":plan_hash,"examined_artifacts":[artifact],"outstanding":"No product implementation remains in this synthetic test","risks":"No production work involved","proposed_next_step":"Retire the test module"})).await;
    live.mutate(&owner,"at_recovery_confirm",&module,json!({"work_id":module,"assignment_id":next.assignment_id,"recovery_report_id":recovery.data["recovery_report_id"],"reason":"Reviewed the synthetic recovery report"})).await;
    live.mutate(&owner,"at_work_retire",&module,json!({"work_id":module,"disposition":"skipped","reason":"The synthetic gate test is complete"})).await;
    live.report["complete"] = json!(true);
    live.report["contract_module_id"] = json!(module);
    live.report["review_scope"] =
        json!("Synthetic test principals, not independent-agent execution");
    std::fs::write(
        &live.report_path,
        serde_json::to_vec_pretty(&live.report).unwrap(),
    )
    .unwrap();
}

/// Verify exact accepted module composition, epic review and native Project completion on real Linear.
#[tokio::test]
#[ignore = "Requires explicit isolated Linear product and a published pilot artifact"]
async fn live_epic_composition() {
    let mut live = Live::new();
    let owner = live.owner();
    let product = live.product.clone();
    let run = live.report["run_id"].as_str().unwrap().to_owned();
    let facts = live.gateway.store.snapshot(&product).await.unwrap();
    assert!(facts.config.payload["pending_operation_key"].is_null());
    let publication = facts
        .records
        .values()
        .find(|r| r.record_kind == "publication")
        .unwrap()
        .clone();
    let document = live
        .gateway
        .store
        .document(&facts, &publication)
        .await
        .unwrap();
    let artifacts = json!([{"kind":"document","locator":document["url"],"sha256":publication.payload["content_hash"]}]);
    let epic=live.mutate(&owner,"at_work_create",&product,json!({"kind":"epic","parent_id":product,"title":format!("Live composition {run}"),"description":"Synthetic live epic composition check","classification":"research"})).await.data["created_work_id"].as_str().unwrap().to_owned();
    let module=live.mutate(&owner,"at_work_create",&epic,json!({"kind":"module","parent_id":epic,"title":"Live epic component","description":"One accepted component in a real Linear Project","classification":"research"})).await.data["created_work_id"].as_str().unwrap().to_owned();
    live.plan(&owner, &module, &[]).await;
    live.plan(&owner, &epic, std::slice::from_ref(&module))
        .await;
    let assignment=live.mutate(&owner,"at_assign",&module,json!({"work_id":module,"principal_id":format!("epic-lead-{run}"),"role":"lead","scope":"Synthetic epic component"})).await;
    let lead = Principal {
        id: format!("epic-lead-{run}"),
        role: Role::Lead,
        products: vec![product.clone()],
        assignment_id: assignment.data["assignment_id"].as_str().map(str::to_owned),
        generation: Some(1),
        epoch: 1,
    };
    live.bind("epic-lead", lead.clone());
    live.begin(&lead, &module).await;
    let context = live
        .call(
            &lead,
            "at_context",
            json!({"product_id":product,"work_id":module,"section":"work"}),
        )
        .await;
    assert_eq!(context.status, "ok");
    let context: Value = serde_json::from_str(context.data["content"].as_str().unwrap()).unwrap();
    let current = live.gateway.store.snapshot(&product).await.unwrap();
    assert_eq!(context["identity"]["primary_parent_id"], epic);
    assert_eq!(
        context["native"]["project"]["id"],
        current.work(&epic).unwrap().identity.payload["epic_project_id"]
    );
    let proof = live.evidence(&artifacts);
    let submission=live.mutate(&lead,"at_submit",&module,json!({"work_id":module,"summary":"Component context and native membership checked","artifacts":artifacts,"evidence":[proof],"reuse_evidence":[],"knowledge_results":[]})).await;
    let case=live.mutate(&owner,"at_review_open",&module,json!({"work_id":module,"submission_id":submission.data["submission_id"],"reviewer_principal":format!("component-reviewer-{run}"),"review_kind":"module","scope":"Synthetic component identity and snapshot checks","required_criteria":["C1"]})).await;
    let reviewer = Principal {
        id: format!("component-reviewer-{run}"),
        role: Role::Reviewer,
        products: vec![product.clone()],
        assignment_id: case.data["reviewer_assignment_id"]
            .as_str()
            .map(str::to_owned),
        generation: Some(1),
        epoch: 1,
    };
    live.bind("component-reviewer", reviewer.clone());
    let checked=live.call(&reviewer,"at_context",json!({"product_id":product,"record_id":submission.data["submission_id"],"section":"record"})).await;
    assert_eq!(checked.status, "ok");
    let checked: Value = serde_json::from_str(checked.data["content"].as_str().unwrap()).unwrap();
    assert_eq!(checked["payload"]["artifacts"], artifacts);
    let proof = live.evidence(&artifacts);
    live.mutate(&reviewer,"at_review_report",&module,json!({"work_id":module,"case_id":case.data["case_id"],"submission_id":submission.data["submission_id"],"coverage":[{"criterion_id":"C1","state":"covered","evidence_ids":[proof["id"]]}],"findings":[],"evidence":[proof],"summary":"Synthetic component review passed"})).await;
    let accepted=live.mutate(&owner,"at_accept",&module,json!({"work_id":module,"submission_id":submission.data["submission_id"],"case_id":case.data["case_id"],"reason":"Synthetic component checks passed"})).await;
    let mut integration = live.evidence(&artifacts);
    integration["kind"] = json!("integration");
    integration["command_or_scenario"] = json!(
        "Read exact accepted child, native epic Project membership and canonical component artifact through real MCP"
    );
    let candidate=live.mutate(&owner,"at_candidate_register",&epic,json!({"epic_id":epic,"kind":"final","included_submissions":[submission.data["submission_id"]],"artifacts":artifacts,"evidence":[integration],"environment":"Real Linear synthetic composition","observation_source":{"kind":"file","locator":live.report_path}})).await;
    assert_eq!(candidate.data["computed_complete"], true);
    let manifest=live.call(&owner,"at_context",json!({"product_id":product,"record_id":candidate.data["candidate_id"],"section":"record"})).await;
    assert_eq!(manifest.status, "ok");
    let manifest: Value = serde_json::from_str(manifest.data["content"].as_str().unwrap()).unwrap();
    assert_eq!(
        manifest["payload"]["accepted_module_result_ids"],
        json!([accepted.data["acceptance_id"]])
    );
    let mut proof = live.evidence(&artifacts);
    proof["kind"] = json!("integration");
    let submitted=live.mutate(&owner,"at_submit",&epic,json!({"work_id":epic,"summary":"Exact accepted component composition checked","artifacts":artifacts,"evidence":[proof],"reuse_evidence":[],"knowledge_results":[],"candidate_id":candidate.data["candidate_id"]})).await;
    let case=live.mutate(&owner,"at_review_open",&epic,json!({"work_id":epic,"submission_id":submitted.data["submission_id"],"reviewer_principal":format!("composition-reviewer-{run}"),"review_kind":"composition","scope":"Synthetic exact component composition","required_criteria":["C1"]})).await;
    let integrator = Principal {
        id: format!("composition-reviewer-{run}"),
        role: Role::Integrator,
        products: vec![product.clone()],
        assignment_id: case.data["reviewer_assignment_id"]
            .as_str()
            .map(str::to_owned),
        generation: Some(1),
        epoch: 1,
    };
    live.bind("composition-reviewer", integrator.clone());
    let context=live.call(&integrator,"at_context",json!({"product_id":product,"record_id":submitted.data["submission_id"],"section":"record"})).await;
    assert_eq!(context.status, "ok");
    let context: Value = serde_json::from_str(context.data["content"].as_str().unwrap()).unwrap();
    assert_eq!(
        context["payload"]["candidate_id"],
        candidate.data["candidate_id"]
    );
    let proof = live.evidence(&artifacts);
    live.mutate(&integrator,"at_review_report",&epic,json!({"work_id":epic,"case_id":case.data["case_id"],"submission_id":submitted.data["submission_id"],"coverage":[{"criterion_id":"C1","state":"covered","evidence_ids":[proof["id"]]}],"findings":[],"evidence":[proof],"summary":"Synthetic composition review passed"})).await;
    let result=live.mutate(&owner,"at_accept",&epic,json!({"work_id":epic,"submission_id":submitted.data["submission_id"],"case_id":case.data["case_id"],"reason":"Live composition and projection verified"})).await;
    assert_eq!(result.data["acceptance_level"], "epic");
    let mut proof = live.evidence(&artifacts);
    proof["kind"] = json!("integration");
    proof["command_or_scenario"] =
        json!("Read the exact accepted Document artifact and compare its content hash");
    live.mutate(&owner,"at_integration_record",&epic,json!({"work_id":epic,"acceptance_id":result.data["acceptance_id"],"target":artifacts[0],"method":"artifact_identity","evidence":proof})).await;
    let final_state = live.gateway.store.snapshot(&product).await.unwrap();
    let project_id = final_state.work(&epic).unwrap().identity.payload["epic_project_id"]
        .as_str()
        .unwrap();
    let native = live
        .gateway
        .store
        .linear
        .object("QProject", "project", project_id)
        .await
        .unwrap();
    assert_eq!(
        native["status"]["id"],
        final_state.config.payload["project_status_ids"]["accepted"]
    );
    live.report["complete"] = json!(true);
    live.report["epic_id"] = json!(epic);
    live.report["native_project_url"] = native["url"].clone();
    live.report["acceptance_id"] = result.data["acceptance_id"].clone();
    live.report["review_scope"] =
        json!("Synthetic role-bound test checks, not independent agent execution");
    std::fs::write(
        &live.report_path,
        serde_json::to_vec_pretty(&live.report).unwrap(),
    )
    .unwrap();
}
