//! Stateful Linear fixture supporting only the operations exercised by workflow tests.
use agent_tasks_linear::{gateway::Gateway, linear::Linear, model::Outcome};
use axum::{Json, Router, extract::State, routing::post};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::Mutex;
use uuid::Uuid;

/// Fresh v4 identifier for a distinct logical request.
pub fn id() -> String {
    Uuid::new_v4().to_string()
}
/// One fixture workspace with inspectable native entities and a one-shot response-loss switch.
#[derive(Default)]
pub struct Database {
    /// Native project objects by UUID.
    pub projects: BTreeMap<String, Value>,
    /// Native issue objects by UUID.
    pub issues: BTreeMap<String, Value>,
    /// Attachment objects, holding durable MCP metadata across gateway restarts.
    pub attachments: BTreeMap<String, Value>,
    /// Native documents by UUID.
    pub documents: BTreeMap<String, Value>,
    /// Native project updates by UUID.
    pub project_updates: BTreeMap<String, Value>,
    /// Native comments by caller UUID.
    pub comments: BTreeMap<String, Value>,
    /// Native workflow labels by UUID.
    pub labels: BTreeMap<String, Value>,
    /// Native directed issue relations, with duplicate relations controlling source status.
    pub relations: BTreeMap<String, Value>,
    /// Simulated monotonic timestamps for completion identity.
    pub tick: u64,
    /// Mutation operation whose response should be lost after applying its write.
    pub lose: Option<String>,
    /// Override only the next created Comment payload body, leaving native storage intact.
    pub comment_response_body: Option<String>,
    /// Return one successful issueUpdate payload without applying its fields.
    pub stale_update: bool,
    /// Serialize unordered list markers like Linear after issue description/comment writes.
    pub normalize_lists: bool,
}
/// Native standard workflow names in the fixture.
pub const STATES: [&str; 7] = [
    "Backlog",
    "Todo",
    "In Progress",
    "In Review",
    "Done",
    "Canceled",
    "Duplicate",
];
/// Build a complete one-page native connection.
fn page(nodes: Vec<Value>) -> Value {
    json!({"nodes":nodes,"pageInfo":{"hasNextPage":false,"endCursor":null}})
}
/// Slice a connection with opaque numeric cursors using the requested native page size.
fn issue_page(nodes: Vec<Value>, variables: &Value) -> Value {
    let first = variables["first"].as_u64().unwrap_or(100) as usize;
    let start = variables["after"]
        .as_str()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    let end = (start + first).min(nodes.len());
    json!({"nodes":nodes[start.min(nodes.len())..end].to_vec(),"pageInfo":{"hasNextPage":end<nodes.len(),"endCursor":if end<nodes.len(){json!(end.to_string())}else{Value::Null}}})
}
/// Resolve a native status object by its fixture identifier.
fn state(id: &str) -> Value {
    json!({"id":id,"name":id,"type":match id {"Backlog"=>"backlog","Todo"=>"unstarted","Done"=>"completed","Canceled"=>"canceled","Duplicate"=>"duplicate",_=>"started"}})
}
/// Mock GraphQL only at the HTTP boundary; the real transport and all workflow code are exercised.
async fn graphql(
    State(db): State<Arc<Mutex<Database>>>,
    Json(request): Json<Value>,
) -> Json<Value> {
    let mut db = db.lock().await;
    let op = request["operationName"].as_str().unwrap();
    let v = &request["variables"];
    let id = v["id"].as_str().unwrap_or("");
    let input = &v["input"];
    if op == "MUpdateIssue" && input["stateId"] == "Duplicate" {
        return Json(
            json!({"data":{"issueUpdate":null},"errors":[{"message":"Duplicate is a system-managed state","extensions":{"code":"INPUT_ERROR"}}]}),
        );
    }
    if op == "MUpdateIssue" && db.stale_update {
        db.stale_update = false;
        return Json(json!({"data":{"issueUpdate":{"success":true,"issue":db.issues[id]}}}));
    }
    let mut data = json!({});
    let found = match op {
        "QViewer" => Some(("viewer", json!({"id":"user","name":"Fixture"}))),
        "QTeam" => Some((
            "team",
            json!({"id":id,"name":"Fixture","autoCloseParentIssues":false,"autoCloseChildIssues":false}),
        )),
        "QStates" => Some((
            "workflowStates",
            page(STATES.iter().map(|s| state(s)).collect()),
        )),
        "QLabels" => Some(("issueLabels", page(db.labels.values().cloned().collect()))),
        "QProject" => db.projects.get(id).cloned().map(|v| ("project", v)),
        "QIssue" => db
            .issues
            .values()
            .find(|v| v["id"] == id || v["identifier"] == id)
            .cloned()
            .map(|v| ("issue", v)),
        "QIssueRelations" => Some((
            "issue",
            json!({"relations":issue_page(db.relations.values().filter(|r| r["issue"]["id"] == id).cloned().collect(),v)}),
        )),
        "QAttachmentById" => db.attachments.get(id).cloned().map(|v| ("attachment", v)),
        "QDocument" => db.documents.get(id).cloned().map(|v| ("document", v)),
        "QComment" => db.comments.get(id).cloned().map(|v| ("comment", v)),
        "QProjectUpdate" => db
            .project_updates
            .get(id)
            .cloned()
            .map(|v| ("projectUpdate", v)),
        "QProjectUpdates" => Some((
            "projectUpdates",
            issue_page(
                db.project_updates
                    .values()
                    .filter(|update| {
                        v["filter"]["project"].is_null()
                            || update["project"]["id"] == v["filter"]["project"]["id"]["eq"]
                    })
                    .cloned()
                    .collect(),
                v,
            ),
        )),
        "QCommentChildren" => db.comments.get(id).map(|_| {
            (
                "comment",
                json!({"children":issue_page(
                    db.comments.values().filter(|c| c["parent"]["id"] == id).cloned().collect(),
                    v
                )}),
            )
        }),
        "QComments" => Some((
            "comments",
            issue_page(
                db.comments
                    .values()
                    .filter(|c| {
                        ["issue", "project", "projectUpdate"].iter().all(|key| {
                            v["filter"][key].is_null()
                                || c[*key]["id"] == v["filter"][key]["id"]["eq"]
                        })
                    })
                    .filter(|c| {
                        if v["filter"]["parent"]["null"] == true {
                            c["parent"].is_null()
                        } else {
                            v["filter"]["parent"].is_null()
                                || c["parent"]["id"] == v["filter"]["parent"]["id"]["eq"]
                        }
                    })
                    .cloned()
                    .collect(),
                v,
            ),
        )),
        "QIssues" => Some((
            "issues",
            issue_page(
                db.issues
                    .values()
                    .filter(|i| {
                        v["filter"]["project"].is_null()
                            || i["project"]["id"] == v["filter"]["project"]["id"]["eq"]
                    })
                    .filter(|i| {
                        v["filter"]["team"].is_null()
                            || i["team"]["id"] == v["filter"]["team"]["id"]["eq"]
                    })
                    .filter(|i| {
                        v["filter"]["state"].is_null()
                            || i["state"]["name"] == v["filter"]["state"]["name"]["eq"]
                    })
                    .filter(|i| {
                        v["filter"]["priority"].is_null()
                            || i["priority"] == v["filter"]["priority"]["eq"]
                    })
                    .filter(|i| {
                        v["filter"]["parent"].is_null()
                            || i["parent"]["id"] == v["filter"]["parent"]["id"]["eq"]
                    })
                    .filter(|i| {
                        v["filter"]["labels"].is_null()
                            || i["labels"]["nodes"].as_array().is_some_and(|ls| {
                                ls.iter().any(|l| {
                                    l["name"] == v["filter"]["labels"]["some"]["name"]["eq"]
                                })
                            })
                    })
                    .cloned()
                    .collect(),
                v,
            ),
        )),
        "QDocuments" => Some((
            "documents",
            page(
                db.documents
                    .values()
                    .filter(|i| {
                        v["filter"]["project"].is_null()
                            || i["project"]["id"] == v["filter"]["project"]["id"]["eq"]
                    })
                    .cloned()
                    .collect(),
            ),
        )),
        "MCreateProject" => {
            let id = input["id"].as_str().unwrap();
            let item = json!({"id":id,"name":input["name"],"content":input["content"],"url":format!("https://linear.app/project/{id}"),"archivedAt":null,"teams":page(input["teamIds"].as_array().unwrap().iter().map(|i|json!({"id":i})).collect())});
            assert!(!db.projects.contains_key(id));
            db.projects.insert(id.into(), item.clone());
            Some(("projectCreate", json!({"success":true,"project":item})))
        }
        "MUpdateProject" => {
            let p = db.projects.get_mut(id).unwrap();
            for (k, v) in input.as_object().unwrap() {
                p[k] = v.clone();
            }
            Some(("projectUpdate", json!({"success":true,"project":p})))
        }
        "MCreateIssueLabel" => {
            let id = input["id"].as_str().unwrap();
            let l = json!({"id":id,"name":input["name"],"team":{"id":input["teamId"]}});
            db.labels.insert(id.into(), l.clone());
            Some(("issueLabelCreate", json!({"success":true,"issueLabel":l})))
        }
        "MCreateIssue" => {
            let id = input["id"].as_str().unwrap();
            assert!(!db.issues.contains_key(id));
            let labels = input["labelIds"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| db.labels[i.as_str().unwrap()].clone())
                .collect();
            let item = json!({"id":id,"identifier":format!("TEST-{}",db.issues.len()+1),"title":input["title"],"description":input["description"],"priority":input.get("priority").cloned().unwrap_or(json!(0)),"priorityLabel":match input["priority"].as_u64().unwrap_or(0){1=>"Urgent",2=>"High",3=>"Medium",4=>"Low",_=>"No priority"},"prioritySortOrder":-(db.issues.len() as i64),"team":{"id":input["teamId"]},"project":{"id":input["projectId"]},"parent":input.get("parentId").filter(|v|!v.is_null()).map(|id|json!({"id":id})),"state":state(input["stateId"].as_str().unwrap()),"url":format!("https://linear.app/issue/{id}"),"labels":page(labels),"archivedAt":null,"startedAt":null,"completedAt":null});
            db.issues.insert(id.into(), item.clone());
            Some(("issueCreate", json!({"success":true,"issue":item})))
        }
        "MUpdateIssue" => {
            db.tick += 1;
            let tick = db.tick;
            let normalize_lists = db.normalize_lists;
            let item = db.issues.get_mut(id).unwrap();
            for (k, v) in input.as_object().unwrap() {
                match k.as_str() {
                    "stateId" => {
                        if item["state"]["name"] != *v {
                            item["state"] = state(v.as_str().unwrap());
                            item["completedAt"] = if v == "Done" {
                                json!(format!("2026-09-25T00:00:{tick:02}Z"))
                            } else {
                                Value::Null
                            };
                        }
                    }
                    "parentId" => {
                        item["parent"] = if v.is_null() {
                            Value::Null
                        } else {
                            json!({"id":v})
                        }
                    }
                    "description" if normalize_lists => {
                        item[k] = json!(
                            v.as_str()
                                .unwrap()
                                .lines()
                                .map(|line| line
                                    .strip_prefix("- ")
                                    .map(|body| format!("* {body}"))
                                    .unwrap_or_else(|| line.to_owned()))
                                .collect::<Vec<_>>()
                                .join("\n")
                        )
                    }
                    _ => item[k] = v.clone(),
                }
            }
            Some(("issueUpdate", json!({"success":true,"issue":item})))
        }
        "MCreateIssueRelation" => {
            assert_eq!(input["type"], "duplicate");
            let source = input["issueId"].as_str().unwrap();
            let target = input["relatedIssueId"].as_str().unwrap();
            assert_ne!(source, target);
            assert!(db.issues.contains_key(target));
            let relation = json!({"id":input["id"],"type":"duplicate","archivedAt":null,"issue":{"id":source},"relatedIssue":{"id":target}});
            assert!(
                !db.relations
                    .values()
                    .any(|r| r["issue"]["id"] == source && r["type"] == "duplicate")
            );
            db.relations
                .insert(input["id"].as_str().unwrap().into(), relation.clone());
            db.issues.get_mut(source).unwrap()["state"] = state("Duplicate");
            for attachment in db
                .attachments
                .values_mut()
                .filter(|a| a["issue"]["id"] == source)
            {
                if attachment["originalIssue"].is_null() {
                    attachment["originalIssue"] = attachment["issue"].clone();
                }
                attachment["issue"] = json!({"id":target});
            }
            Some((
                "issueRelationCreate",
                json!({"success":true,"issueRelation":relation}),
            ))
        }
        "MUpsertRecord" => {
            let aid = input["id"].as_str().unwrap();
            assert!(!db.attachments.contains_key(aid));
            let item = json!({"id":aid,"metadata":input["metadata"],"issue":{"id":input["issueId"]},"title":input["title"],"url":input["url"]});
            db.attachments.insert(aid.into(), item.clone());
            Some((
                "attachmentCreate",
                json!({"success":true,"attachment":item}),
            ))
        }
        "MUpdateAttachment" => {
            let item = db.attachments.get_mut(id).unwrap();
            item["metadata"] = input["metadata"].clone();
            Some((
                "attachmentUpdate",
                json!({"success":true,"attachment":item}),
            ))
        }
        "MCreateDocument" => {
            let id = input["id"].as_str().unwrap();
            assert!(!db.documents.contains_key(id));
            let item = json!({"id":id,"title":input["title"],"content":input["content"],"url":format!("https://linear.app/document/{id}"),"project":input.get("projectId").map(|id|json!({"id":id})),"issue":input.get("issueId").map(|id|json!({"id":id}))});
            db.documents.insert(id.into(), item.clone());
            Some(("documentCreate", json!({"success":true,"document":item})))
        }
        "MUpdateDocument" => {
            let item = db.documents.get_mut(id).unwrap();
            for (k, v) in input.as_object().unwrap() {
                item[k] = v.clone();
            }
            Some(("documentUpdate", json!({"success":true,"document":item})))
        }
        "MCreateProjectUpdate" => {
            let id = input["id"].as_str().unwrap();
            let project = input["projectId"].as_str().unwrap();
            assert!(db.projects.contains_key(project));
            assert!(!db.project_updates.contains_key(id));
            db.tick += 1;
            let item = json!({
                "id":id,"url":format!("https://linear.app/project/{project}/updates/{id}"),
                "body":input["body"],"health":input["health"],
                "createdAt":format!("2026-09-25T00:00:{:02}Z",db.tick),
                "updatedAt":format!("2026-09-25T00:00:{:02}Z",db.tick),
                "archivedAt":null,"project":{"id":project},"user":{"id":"fixture","name":"Fixture"}
            });
            db.project_updates.insert(id.into(), item.clone());
            Some((
                "projectUpdateCreate",
                json!({"success":true,"projectUpdate":item}),
            ))
        }
        "MUpdateProjectUpdate" => {
            db.tick += 1;
            let tick = db.tick;
            let item = db.project_updates.get_mut(id).unwrap();
            item["body"] = input["body"].clone();
            item["health"] = input["health"].clone();
            item["updatedAt"] = json!(format!("2026-09-25T00:00:{tick:02}Z"));
            Some((
                "projectUpdateUpdate",
                json!({"success":true,"projectUpdate":item}),
            ))
        }
        "MCreateComment" => {
            let id = input["id"].as_str().unwrap();
            assert!(!db.comments.contains_key(id));
            let target = if let Some(target) = input["issueId"].as_str() {
                db.issues[target]["url"].as_str().unwrap()
            } else if let Some(target) = input["projectId"].as_str() {
                db.projects[target]["url"].as_str().unwrap()
            } else {
                db.project_updates[input["projectUpdateId"].as_str().unwrap()]["url"]
                    .as_str()
                    .unwrap()
            }
            .to_owned();
            db.tick += 1;
            let body = if db.normalize_lists {
                input["body"]
                    .as_str()
                    .unwrap()
                    .lines()
                    .map(|line| {
                        line.strip_prefix("- ")
                            .map(|body| format!("* {body}"))
                            .unwrap_or_else(|| line.to_owned())
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                input["body"].as_str().unwrap().to_owned()
            };
            let url = if let Some(update_id) = input["projectUpdateId"].as_str() {
                let project_id = db.project_updates[update_id]["project"]["id"]
                    .as_str()
                    .unwrap();
                let project_url = db.projects[project_id]["url"].as_str().unwrap();
                format!(
                    "{project_url}/activity#project-update-{update_id}&comment-{}",
                    &id[..8]
                )
            } else {
                format!("{target}#comment-{}", &id[..8])
            };
            let item = json!({
                "id":id,"url":url,"body":body,
                "issue":input.get("issueId").map(|id|json!({"id":id})),
                "project":input.get("projectId").map(|id|json!({"id":id})),
                "projectUpdate":input.get("projectUpdateId").map(|id|json!({"id":id})),
                "parent":input.get("parentId").map(|id|json!({"id":id})),
                "createdAt":format!("2026-09-25T00:00:{:02}Z",db.tick),
                "updatedAt":format!("2026-09-25T00:00:{:02}Z",db.tick),
                "resolvedAt":null,"resolvingCommentId":null,"user":{"id":"fixture","name":"Fixture"}
            });
            db.comments.insert(id.into(), item.clone());
            let mut returned = item;
            if let Some(body) = db.comment_response_body.take() {
                returned["body"] = json!(body);
            }
            Some(("commentCreate", json!({"success":true,"comment":returned})))
        }
        "MResolveComment" | "MUnresolveComment" => {
            db.tick += 1;
            let tick = db.tick;
            let item = db.comments.get_mut(id).unwrap();
            item["resolvedAt"] = if op == "MResolveComment" {
                json!(format!("2026-09-25T00:00:{tick:02}Z"))
            } else {
                Value::Null
            };
            item["resolvingCommentId"] = if op == "MResolveComment" {
                v["resolvingCommentId"].clone()
            } else {
                Value::Null
            };
            Some((
                if op == "MResolveComment" {
                    "commentResolve"
                } else {
                    "commentUnresolve"
                },
                json!({"success":true,"comment":item}),
            ))
        }
        _ => panic!("Unimplemented fixture operation: {op}"),
    };
    if db.lose.as_deref() == Some(op) {
        db.lose = None;
        return Json(json!({"errors":[{"message":"simulated response loss"}]}));
    }
    if let Some((k, v)) = found {
        data[k] = v;
        Json(json!({"data":data}))
    } else {
        Json(
            json!({"errors":[{"message":"Entity not found: Fixture","extensions":{"code":"INPUT_ERROR","type":"invalid input"}}]}),
        )
    }
}
/// Own the mock server and expose a fresh gateway plus durable backing data.
pub struct Fixture {
    /// Gateway under test.
    pub gateway: Arc<Gateway>,
    /// Native state inspectable for drift and failure injection.
    pub db: Arc<Mutex<Database>>,
    /// Loopback API endpoint reused after cold restarts.
    endpoint: String,
    /// Mock service task, stopped on drop.
    task: tokio::task::JoinHandle<()>,
    /// Fixture team UUID.
    pub team: String,
}
impl Fixture {
    /// Start the native HTTP fixture without any real credentials.
    pub async fn new() -> Self {
        let db = Arc::new(Mutex::new(Database::default()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/", post(graphql))
            .with_state(db.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            gateway: Gateway::new(Linear::mock(&endpoint).unwrap()).unwrap(),
            db,
            endpoint,
            task,
            team: id(),
        }
    }
    /// Replace all process memory while retaining only native Linear data.
    pub fn restart(&mut self) {
        self.gateway = Gateway::new(Linear::mock(&self.endpoint).unwrap()).unwrap();
    }
    /// Call a public tool, supplying stable attribution and a new request ID if absent.
    pub async fn call(&self, name: &str, mut args: Value) -> Outcome {
        if !matches!(
            name,
            "get_context" | "get_comment" | "list_items" | "search"
        ) {
            if args.get("request_id").is_none() {
                args["request_id"] = json!(id())
            }
            args["actor"] = json!("codex:fixture");
        }
        self.gateway.call(name, args).await
    }
    /// Require success and return its payload; failures identify the exact tool.
    pub async fn ok(&self, name: &str, args: Value) -> Value {
        let r = self.call(name, args).await;
        assert_eq!(r.status, "ok", "{name}: {}", r.data);
        r.data
    }
    /// Create a readable Project with its default documentation.
    pub async fn project(&self) -> String {
        let p=self.ok("create_project",json!({"team_id":self.team,"title":"Example product","description":"Workflow integration fixture","repository_url":"https://github.com/example/product"})).await;
        p["project"]["id"].as_str().unwrap().into()
    }
    /// Create work with fully prepared generic fields appropriate to its kind.
    pub async fn work(&self, kind: &str, project: &str, parent: Option<&str>) -> String {
        let mut fields = json!({"description":"Human readable work","expected_result":"Observable result","acceptance_criteria":"Scenarios pass"});
        match kind {
            "epic" => fields["business_requirements"] = json!("Business outcome"),
            "module" => {
                fields["lead"] = json!("codex:lead");
                fields["branch"] = json!("feature/example");
                fields["worktree"] = json!("/tmp/example");
                fields["required_contract"] = json!("Not required");
                fields["provided_contract"] = json!("Documented API");
            }
            _ => {
                fields["work_type"] = json!("non_code");
                fields["local_check"] = json!("Inspect output");
                fields["executor"] = json!("codex:worker");
            }
        }
        let item=self.ok(&format!("create_{kind}"),json!({"team_id":self.team,"project_id":project,"parent_id":parent,"title":format!("Readable {kind}"),"fields":fields})).await;
        item["issue"]["id"].as_str().unwrap().into()
    }
    /// Explicit orchestrator transition with fresh request attribution.
    pub async fn mv(&self, id: &str, status: &str) -> Value {
        self.ok(
            "move_status",
            json!({"id":id,"status":status,"actor_role":"orchestrator"}),
        )
        .await
    }
    /// Patch implementation outputs for an ordinary Task/Module/Epic/Atomic.
    pub async fn result(&self, kind: &str, id: &str) {
        let mut fields =
            json!({"result":"Implemented expected result","check_result":"Local scenarios passed"});
        if kind == "module" {
            fields["pr_url"] = json!("https://github.com/example/product/pull/1");
        } else {
            fields["artifact_url"] = json!("https://example.com/report");
        }
        self.ok(&format!("edit_{kind}"), json!({"id":id,"fields":fields}))
            .await;
    }
    /// Submit one independently attributed accepted or changes-requested review.
    pub async fn review(&self, id: &str, verdict: &str) {
        self.ok("record_review",json!({"id":id,"reviewer":"codex:reviewer","verdict":verdict,"summary":"Checked behavior and artifacts","findings":"","artifacts":["https://example.com/report"]})).await;
    }
    /// Finish a coding Module through review and reported merge, without task-level reviews.
    pub async fn finish_module(&self, id: &str) {
        self.result("module", id).await;
        self.mv(id, "In Review").await;
        self.review(id, "accepted").await;
        self.ok(
            "edit_module",
            json!({"id":id,"fields":{"merge_report":"PR merged into main"}}),
        )
        .await;
        self.mv(id, "Done").await;
    }
}
impl Drop for Fixture {
    /// Stop the mock HTTP service once its owning test ends.
    fn drop(&mut self) {
        self.task.abort();
    }
}
