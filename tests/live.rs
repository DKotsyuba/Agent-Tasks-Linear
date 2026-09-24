//! Explicitly opted-in acceptance against an isolated real Linear product; never runs by default.

use agent_tasks_linear::{
    config::Config,
    gateway::Gateway,
    linear::Linear,
    model::{Outcome, Principal, Role},
    records::{Signer, Store},
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
    /// Retain each logical call key so an interrupted pilot can resume without duplicate work.
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
    /// Add only the product and retry key; activity calls require no plan or evidence tokens.
    async fn arguments(&self, _principal: &Principal, _work: &str, mut args: Value) -> Value {
        args["product_id"] = json!(self.product);
        args["idempotency_key"] = json!(Uuid::new_v4().to_string());
        args
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

/// Exercise the short trusted-agent cycle against an explicitly configured disposable Linear product.
#[tokio::test]
#[ignore = "Requires an authorized disposable product, protected config and LINEAR_API_KEY"]
async fn live_activity_cycle() {
    let mut live = Live::new();
    let owner = live.owner();
    let product = live.product.clone();
    let run = live.report["run_id"].as_str().unwrap().to_owned();
    let repository = std::env::current_dir().unwrap();
    let commit = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let commit = String::from_utf8(commit.stdout).unwrap().trim().to_owned();
    let branch = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .output()
        .unwrap();
    let branch = String::from_utf8(branch.stdout).unwrap().trim().to_owned();
    let module = live
        .mutate(
            &owner,
            "at_work_create",
            &product,
            json!({"kind":"module","parent_id":product,
        "title":format!("Activity cycle {run}"),"description":"Trusted agent activity pilot"}),
        )
        .await;
    let module = module.data["created_work_id"].as_str().unwrap().to_owned();
    let task = live
        .mutate(
            &owner,
            "at_work_create",
            &module,
            json!({"kind":"task","parent_id":module,
        "title":"Record agent, checkout and result links"}),
        )
        .await;
    let task = task.data["created_work_id"].as_str().unwrap().to_owned();
    let principal_id = format!("activity-lead-{run}");
    let assigned = live
        .mutate(
            &owner,
            "at_assign",
            &module,
            json!({"work_id":module,"principal_id":principal_id}),
        )
        .await;
    let lead = Principal {
        id: principal_id.clone(),
        role: Role::Lead,
        products: vec![product.clone()],
        assignment_id: assigned.data["assignment_id"].as_str().map(str::to_owned),
        generation: Some(1),
        epoch: owner.epoch,
    };
    live.bind("activity-lead", lead.clone());
    let execution = json!({"repository":repository,"branch":branch,"worktree":repository,"agent":principal_id,
        "runtime":"cargo-test","run_id":run,"run_url":format!("file:{}",live.report_path.display())});
    live.mutate(
        &lead,
        "at_begin",
        &task,
        json!({"work_id":task,"execution":execution}),
    )
    .await;
    let artifacts = json!([{"kind":"git_commit","locator":repository,"commit":commit},
        {"kind":"file","locator":live.report_path}]);
    live.mutate(&lead,"at_checkpoint",&task,json!({"work_id":task,"summary":"Recorded the running test client and checkout","artifacts":artifacts})).await;
    let done = live
        .mutate(
            &lead,
            "at_complete",
            &task,
            json!({"work_id":task,"summary":"Activity trace recorded","artifacts":artifacts}),
        )
        .await;
    assert_eq!(done.data["state"], "accepted");
    let context = live
        .call(
            &lead,
            "at_context",
            json!({"product_id":product,"work_id":task,"section":"work"}),
        )
        .await;
    assert_eq!(context.status, "ok");
    let content: Value = serde_json::from_str(context.data["content"].as_str().unwrap()).unwrap();
    assert_eq!(content["activity"]["execution"], execution);
    assert_eq!(content["activity"]["completed_by"], principal_id);
    assert_eq!(content["head"]["result_id"], done.data["result_id"]);
    let cold = live.cold_resume(&task).await;
    assert_eq!(cold.status, "ok", "{cold:?}");
    assert_eq!(
        cold.data["items"][0]["activity"]["execution"]["run_id"],
        run
    );
    assert_eq!(cold.data["items"][0]["activity"]["artifacts"], artifacts);
    live.mutate(&lead,"at_complete",&module,json!({"work_id":module,"summary":"Live activity pilot completed","artifacts":artifacts,"execution":execution})).await;
    live.report["complete"] = json!(true);
    live.report["commit"] = json!(commit);
    live.report["module_id"] = json!(module);
    live.report["task_url"] = content["native"]["url"].clone();
    live.report["cold_resume"] = serde_json::to_value(cold).unwrap();
    let serialized = serde_json::to_string_pretty(&live.report).unwrap();
    assert!(!serialized.contains(&live.config.signing_key));
    for binding in &live.config.bindings {
        assert!(!serialized.contains(&binding.token));
    }
    std::fs::write(&live.report_path, serialized).unwrap();
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
