//! Explicit workflow operations over native Linear entities and local Git source snapshots.
use crate::{
    catalog::Catalog,
    linear::Linear,
    model::{Fault, Kind, Meta, Outcome, Pending, Result, Review, Status, Work, require, text},
    records::{Store, child_id, markdown_equivalent, markdown_key, patch_description, read_fields},
    rules,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex as StdMutex},
    time::Instant,
};
use tokio::sync::Mutex;

/// Read a native priority that is represented as an integral JSON number from 0 through 4.
fn native_priority(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        value
            .as_f64()
            .filter(|n| (0.0..=4.0).contains(n) && n.fract() == 0.0)
            .map(|n| n as u64)
    })
}

/// Compare owned native IDs, relationships, title, labels and Markdown description, ignoring timestamps and normalizing native presentation against the requested source; compare priority only when the target carries it.
/// Unknown description sections still participate, so retries reject changes that could erase newly added human text; older snapshots without priority remain compatible.
fn same_native(a: &Value, b: &Value) -> bool {
    ["id", "title", "archivedAt"]
        .iter()
        .all(|key| a[key] == b[key])
        && ["parent", "project", "team", "state"]
            .iter()
            .all(|key| a[key]["id"] == b[key]["id"])
        && a["labels"] == b["labels"]
        && (b.get("priority").is_none()
            || native_priority(&a["priority"]) == native_priority(&b["priority"]))
        && markdown_equivalent(
            b["description"].as_str().unwrap_or(""),
            a["description"].as_str().unwrap_or(""),
        )
}

/// Order native Issue objects by priority 1–4 then 0, ascending `prioritySortOrder`, and ascending UUID; missing or unknown priorities rank with 0 and missing tie order follows known values.
pub(crate) fn priority_cmp(a: &Value, b: &Value) -> std::cmp::Ordering {
    /// Map native priority zero/unknown to the final advisory bucket.
    fn rank(v: &Value) -> u64 {
        match native_priority(&v["priority"]).unwrap_or(0) {
            1..=4 => native_priority(&v["priority"]).unwrap(),
            _ => 5,
        }
    }
    rank(a)
        .cmp(&rank(b))
        .then_with(|| {
            a["prioritySortOrder"]
                .as_f64()
                .unwrap_or(f64::MAX)
                .total_cmp(&b["prioritySortOrder"].as_f64().unwrap_or(f64::MAX))
        })
        .then_with(|| {
            a["id"]
                .as_str()
                .unwrap_or("")
                .cmp(b["id"].as_str().unwrap_or(""))
        })
}

/// One writer shared by HTTP clients and stdio bridges. No background work is performed.
pub struct Gateway {
    /// Discoverable, strictly validated tool surface.
    pub catalog: Catalog,
    /// Native persistence, with no authoritative in-memory workflow cache.
    pub store: Store,
    /// ponytail: serialize requests across this gateway; use project locks only if throughput requires it.
    lock: Mutex<()>,
    /// Bounded process-local comparison points; native Linear data remains authoritative.
    baselines: StdMutex<crate::context::SnapshotCache>,
}
impl Gateway {
    /// Construct a gateway without contacting Linear; tool discovery works before credentials exist.
    pub fn new(linear: Linear) -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            catalog: Catalog::new()?,
            store: Store { linear },
            lock: Mutex::new(()),
            baselines: StdMutex::new(crate::context::SnapshotCache::default()),
        }))
    }
    /// Validate, serialize and dispatch one tool call, retaining uncertain write outcomes.
    pub async fn call(&self, name: &str, args: Value) -> Outcome {
        if let Err(e) = self.catalog.validate(name, &args) {
            return Outcome::failure(e);
        }
        let _guard = self.lock.lock().await;
        match self.dispatch(name, args).await {
            Ok(v) => Outcome::ok(v),
            Err(e) => Outcome::failure(e),
        }
    }
    /// Map the closed tool vocabulary to native operations; no raw GraphQL tool is exposed.
    async fn dispatch(&self, name: &str, args: Value) -> Result<Value> {
        match name {
            "create_project" => self.create_project(&args).await,
            "edit_project" => self.edit_project(&args).await,
            "get_context" => self.context(&args).await,
            "get_overview" => self.overview(&args).await,
            "list_items" => self.list(&args, false).await,
            "search" => self.list(&args, true).await,
            "save_document" => self.document(&args).await,
            "move_status" => self.move_status(&args).await,
            "record_review" => self.review(&args).await,
            "record_commits" => self.record_commits(&args).await,
            "add_comment" => self.add_comment(&args).await,
            "get_comment" => self.get_comment(&args).await,
            "resolve_comment" => self.resolve_comment(&args).await,
            "save_project_update" => self.save_project_update(&args).await,
            _ => {
                let (action, kind) = name
                    .split_once('_')
                    .ok_or_else(|| Fault::new("UNKNOWN_TOOL", name))?;
                let kind: Kind = serde_json::from_value(json!(kind))
                    .map_err(|_| Fault::new("UNKNOWN_TOOL", name))?;
                if action == "create" {
                    self.create_work(kind, &args).await
                } else {
                    self.edit_work(kind, &args).await
                }
            }
        }
    }
    /// Require an active native project and return its readable content.
    async fn project(&self, id: &str) -> Result<Value> {
        let p = self.store.linear.object("QProject", "project", id).await?;
        require(
            p["archivedAt"].is_null(),
            "ARCHIVED_ITEM",
            "Project is archived",
        )?;
        Ok(p)
    }
    /// Verify a team's native automation cannot cascade parent/child statuses, then resolve existing states.
    async fn states(&self, team: &str) -> Result<Vec<Value>> {
        let team_data = self.store.linear.object("QTeam", "team", team).await?;
        require(
            team_data.get("autoCloseParentIssues").is_some()
                && team_data.get("autoCloseChildIssues").is_some()
                && team_data["autoCloseParentIssues"] != true
                && team_data["autoCloseChildIssues"] != true,
            "TEAM_AUTOMATION_ENABLED",
            "Disable automatic parent/child closure in Linear team settings before using guarded workflow",
        )?;
        self.store
            .pages(
                "QStates",
                "workflowStates",
                json!({"filter":{"team":{"id":{"eq":team}}}}),
            )
            .await
    }
    /// Resolve an existing standard workflow status without provisioning duplicate states.
    fn state_id(states: &[Value], status: Status) -> Result<String> {
        let values: Vec<_> = states
            .iter()
            .filter(|s| s["name"] == status.name())
            .collect();
        require(
            values.len() == 1,
            "WORKFLOW_CONFIGURATION",
            format!("Expected one existing {} state", status.name()),
        )?;
        Ok(values[0]["id"].as_str().unwrap().into())
    }
    /// Reuse the team's or workspace's exact type label; create a missing label with a stable UUID.
    async fn label(&self, team: &str, kind: Kind) -> Result<String> {
        let labels = self
            .store
            .pages("QLabels", "issueLabels", json!({}))
            .await?;
        let matches: Vec<_> = labels
            .iter()
            .filter(|l| {
                l["name"] == kind.label() && (l["team"].is_null() || l["team"]["id"] == team)
            })
            .collect();
        if let Some(l) = matches
            .iter()
            .find(|l| l["team"]["id"] == team)
            .or(matches.first())
        {
            return Ok(l["id"].as_str().unwrap().into());
        }
        let id = child_id(team, kind.label());
        self.store
            .linear
            .call(
                "MCreateIssueLabel",
                json!({"input":{"id":id,"teamId":team,"name":kind.label(),"color":"#6B7280"}}),
            )
            .await?;
        Ok(id)
    }
    /// Create a permanent native project with optional local repository path and HTTP(S) URL.
    /// Planning needs neither; a supplied path must be an existing Git checkout.
    /// Ensures both default documents on every retry and rejects conflicting same-ID content.
    async fn create_project(&self, a: &Value) -> Result<Value> {
        let id = text(a, "request_id")?;
        let title = text(a, "title")?;
        let description = text(a, "description")?;
        if let Some(path) = a["repository_path"].as_str() {
            crate::git::validate_repository(path)?;
        }
        self.states(text(a, "team_id")?).await?;
        let content = patch_description(
            "",
            &json!({"description":description,"repository_path":a["repository_path"],"repository_url":a["repository_url"]}),
        );
        let project = if let Some(p) = self.store.optional("QProject", "project", id).await? {
            require(
                p["name"] == title
                    && markdown_key(p["content"].as_str().unwrap_or("")) == markdown_key(&content),
                "REQUEST_CONFLICT",
                "Existing Project differs from this create request; edit it explicitly",
            )?;
            p
        } else {
            self.store.linear.call("MCreateProject",json!({"input":{"id":id,"name":title,"teamIds":[a["team_id"]],"content":content}})).await?["projectCreate"]["project"].clone()
        };
        let mut docs = vec![];
        for (purpose, name, body) in [
            (
                "runbook",
                "Runbook",
                "## Запуск\n\n## Проверка\n\n## Восстановление\n",
            ),
            (
                "decisions",
                "Решения",
                "## Принятые решения\n\nФиксируйте решение, причину и дату.\n",
            ),
        ] {
            let did = child_id(id, purpose);
            let d = if let Some(d) = self.store.optional("QDocument", "document", &did).await? {
                require(
                    d["project"]["id"] == id,
                    "REQUEST_CONFLICT",
                    "Default document belongs elsewhere",
                )?;
                d
            } else {
                self.store
                    .linear
                    .call(
                        "MCreateDocument",
                        json!({"input":{"id":did,"projectId":id,"title":name,"content":body}}),
                    )
                    .await?["documentCreate"]["document"]
                    .clone()
            };
            docs.push(d);
        }
        Ok(json!({"project":project,"documents":docs}))
    }
    /// Patch requested project fields, preserving omitted sections, prose and documents.
    /// Null removes either repository field; supplied paths must be existing local Git checkouts.
    /// Returns the native project or a validation/API fault without changing work statuses.
    async fn edit_project(&self, a: &Value) -> Result<Value> {
        let id = text(a, "id")?;
        let p = self.project(id).await?;
        let mut input = json!({});
        if let Some(v) = a.get("title") {
            input["name"] = v.clone();
        }
        let mut fields = json!({});
        if let Some(path) = a["repository_path"].as_str() {
            crate::git::validate_repository(path)?;
        }
        for key in ["description", "repository_path", "repository_url"] {
            if let Some(v) = a.get(key) {
                fields[key] = v.clone();
            }
        }
        if !fields.as_object().unwrap().is_empty() {
            input["content"] = json!(patch_description(
                p["content"].as_str().unwrap_or(""),
                &fields
            ));
        }
        require(
            !input.as_object().unwrap().is_empty(),
            "INVALID_INPUT",
            "No project fields to edit",
        )?;
        Ok(self
            .store
            .linear
            .call("MUpdateProject", json!({"id":id,"input":input}))
            .await?["projectUpdate"]["project"]
            .clone())
    }
    /// Create or edit one native ProjectUpdate with explicit health, author and rationale.
    /// An omitted creation body is composed from a fresh read-only overview on this explicit write;
    /// edits require an explicit body so their retries cannot generate a different request.
    /// New updates use request_id as native ID; edits compare expected_updated_at before writing,
    /// and identical native content makes a lost response replay safe without a second state store.
    async fn save_project_update(&self, a: &Value) -> Result<Value> {
        let project_id = text(a, "project_id")?;
        self.project(project_id).await?;
        let health = text(a, "health")?;
        let actor = text(a, "actor")?;
        let reason = text(a, "reason")?;
        let editing = a["id"].is_string();
        require(
            !editing || a["body"].is_string(),
            "INVALID_INPUT",
            "Edits need an explicit body so the same request remains replayable",
        )?;
        let id = if editing {
            text(a, "id")?
        } else {
            require(
                a.get("expected_updated_at").is_none(),
                "INVALID_INPUT",
                "Creation has no prior update timestamp",
            )?;
            text(a, "request_id")?
        };
        let existing = self
            .store
            .optional("QProjectUpdate", "projectUpdate", id)
            .await?;
        if !editing
            && a["body"].is_null()
            && let Some(current) = &existing
        {
            let record = crate::activity::project_update_record(current)?;
            require(
                current["project"]["id"] == project_id
                    && current["health"] == health
                    && record.actor.as_deref() == Some(actor)
                    && record.reason.as_deref() == Some(reason),
                "REQUEST_CONFLICT",
                "request_id already names another ProjectUpdate",
            )?;
            return Ok(
                json!({"project_update":current,"activity":record,"url":current["url"],"replayed":true}),
            );
        }
        let generated = if a["body"].is_null() {
            Some(self.overview_data(project_id).await?.0)
        } else {
            None
        };
        let source = a["body"]
            .as_str()
            .or_else(|| {
                generated
                    .as_ref()
                    .and_then(|overview| overview["project_update_draft"].as_str())
            })
            .ok_or_else(|| Fault::new("INCOMPLETE_DATA", "ProjectUpdate draft is missing"))?;
        let body = crate::activity::render_project_update(actor, reason, source)?;
        if let Some(current) = &existing {
            require(
                current["project"]["id"] == project_id,
                "REQUEST_CONFLICT",
                "ProjectUpdate belongs to another Project",
            )?;
            if current["health"] == health
                && markdown_key(current["body"].as_str().unwrap_or("")) == markdown_key(&body)
            {
                return Ok(json!({
                    "project_update":current,
                    "activity":crate::activity::project_update_record(current)?,
                    "url":current["url"],"replayed":true
                }));
            }
            require(
                editing,
                "REQUEST_CONFLICT",
                "request_id already names another ProjectUpdate",
            )?;
            require(
                current["updatedAt"] == text(a, "expected_updated_at")?,
                "PENDING_CONFLICT",
                "ProjectUpdate changed since it was read; preserve the concurrent edit",
            )?;
        } else {
            require(
                !editing,
                "RECORD_MISSING",
                "ProjectUpdate to edit is missing",
            )?;
        }
        let native =
            if editing {
                self.store
                    .linear
                    .call(
                        "MUpdateProjectUpdate",
                        json!({"id":id,"input":{"health":health,"body":body}}),
                    )
                    .await?["projectUpdateUpdate"]["projectUpdate"]
                    .clone()
            } else {
                self.store.linear.call(
                "MCreateProjectUpdate",
                json!({"input":{"id":id,"projectId":project_id,"health":health,"body":body}}),
            ).await?["projectUpdateCreate"]["projectUpdate"].clone()
            };
        require(
            native["id"] == id
                && native["project"]["id"] == project_id
                && native["health"] == health
                && markdown_key(native["body"].as_str().unwrap_or("")) == markdown_key(&body),
            "NATIVE_STATE_MISMATCH",
            "Linear did not confirm ProjectUpdate content, health and ownership",
        )
        .map_err(Fault::uncertain)?;
        Ok(json!({
            "project_update":native,
            "activity":crate::activity::project_update_record(&native).map_err(Fault::uncertain)?,
            "url":native["url"],"replayed":false
        }))
    }

    /// Create a native issue, then its small attachment. Same-ID retries resume incomplete creation.
    /// Modules and standalone code Atomics inherit omitted repository fields from Project;
    /// legacy repository prose is not inherited as a URL. Tasks use their Module's checkout.
    async fn create_work(&self, kind: Kind, a: &Value) -> Result<Value> {
        let id = text(a, "request_id")?;
        let project = text(a, "project_id")?;
        let team = text(a, "team_id")?;
        let title = kind.title(text(a, "title")?)?;
        let priority = a.get("priority").and_then(Value::as_u64).unwrap_or(0);
        let p = self.project(project).await?;
        require(
            p["teams"]["nodes"]
                .as_array()
                .is_some_and(|v| v.iter().any(|t| t["id"] == team)),
            "INVALID_PARENT",
            "Team must belong to Project",
        )?;
        if let Some(existing) = self.store.optional("QIssue", "issue", id).await?
            && let Some(meta) = self.store.meta(id).await?
        {
            require(
                meta.kind == kind && meta.creation == *a,
                "REQUEST_CONFLICT",
                "request_id already belongs to another creation",
            )?;
            require(
                existing["title"] == title
                    && native_priority(&existing["priority"]) == Some(priority),
                "REQUEST_CONFLICT",
                "Created Issue no longer matches its canonical title and priority",
            )?;
            self.register_child(meta.parent_id.as_deref(), id, true)
                .await?;
            return Ok(json!({"issue":existing,"replayed":true}));
        }
        let graph = self.store.graph(project).await?;
        let parent = a["parent_id"].as_str();
        rules::enforce(rules::hierarchy(kind, parent, &graph, None))?;
        if let Some(id) = parent {
            let parent = rules::find(&graph, id).unwrap();
            require(
                parent.native["project"]["id"] == project,
                "INVALID_PARENT",
                "Parent must belong to this Project",
            )?;
            rules::enforce(rules::discrepancies(parent, &graph))?;
            require(
                !parent.status()?.terminal(),
                "PARENT_CLOSED",
                "Reopen parent before adding work",
            )?;
        }
        let states = self.states(team).await?;
        let initial = if kind == Kind::Module && parent.is_none() {
            Status::Todo
        } else {
            Status::Backlog
        };
        let state = Self::state_id(&states, initial)?;
        let label = self.label(team, kind).await?;
        let mut fields = a.get("fields").cloned().unwrap_or(json!({}));
        if fields.get("work_type").is_none() {
            fields["work_type"] = json!(if kind == Kind::Epic {
                "non_code"
            } else {
                "code"
            });
        }
        if kind == Kind::Module
            || (kind == Kind::Atomic
                && fields["work_type"] == "code"
                && parent
                    .and_then(|id| rules::find(&graph, id))
                    .is_none_or(|w| w.meta.as_ref().is_none_or(|m| m.kind != Kind::Module)))
        {
            let project_fields = read_fields(p["content"].as_str().unwrap_or(""))?;
            for key in ["repository_path", "repository_url"] {
                if fields.get(key).is_none()
                    && let Some(value) = project_fields.get(key)
                    && self
                        .catalog
                        .validate_fields(kind, &json!({key:value}))
                        .is_ok()
                {
                    fields[key] = value.clone();
                }
            }
        }
        self.catalog.validate_fields(kind, &fields)?;
        if let Some(path) = fields["repository_path"].as_str() {
            crate::git::validate_repository(path)?;
        }
        Self::check_fields(kind, &fields, parent, &graph)?;
        let description = patch_description("", &fields);
        let native = if let Some(existing) = self.store.optional("QIssue", "issue", id).await? {
            require(
                existing["project"]["id"] == project
                    && existing["team"]["id"] == team
                    && existing["title"] == title
                    && native_priority(&existing["priority"]) == Some(priority)
                    && markdown_key(existing["description"].as_str().unwrap_or(""))
                        == markdown_key(&description)
                    && existing["parent"]["id"].as_str() == parent
                    && existing["state"]["id"] == state,
                "REQUEST_CONFLICT",
                "Partially created Issue does not match this request",
            )?;
            existing
        } else {
            self.store.linear.call("MCreateIssue",json!({"input":{"id":id,"title":title,"priority":priority,"description":description,"projectId":project,"teamId":team,"parentId":a.get("parent_id").unwrap_or(&Value::Null),"stateId":state,"labelIds":[label]}})).await?["issueCreate"]["issue"].clone()
        };
        require(
            native["title"] == title && native_priority(&native["priority"]) == Some(priority),
            "NATIVE_STATE_MISMATCH",
            "Linear did not confirm the canonical title and priority",
        )
        .map_err(Fault::uncertain)?;
        let actual = native["description"]
            .as_str()
            .unwrap_or(&description)
            .to_owned();
        let meta = Meta {
            schema: 2,
            kind,
            fields,
            project_id: project.into(),
            parent_id: parent.map(str::to_owned),
            children: vec![],
            status: initial,
            round: 0,
            revision: 1,
            frozen_modules: None,
            integration: BTreeMap::new(),
            git_reports: vec![],
            review: None,
            completed_at: None,
            description: actual,
            creation: a.clone(),
            last_request: None,
            pending: None,
        };
        self.store
            .save(&native, &meta)
            .await
            .map_err(Fault::uncertain)?;
        self.register_child(parent, id, true)
            .await
            .map_err(Fault::uncertain)?;
        Ok(json!({"issue":native,"kind":kind}))
    }
    /// Record a known native child on its parent, preserving other state and refusing pending parent writes.
    /// Repeated registration/removal is a no-op; it changes no native relationship or status.
    async fn register_child(&self, parent: Option<&str>, child: &str, present: bool) -> Result<()> {
        let Some(id) = parent else { return Ok(()) };
        let w = self.store.work(id).await?;
        let mut m = w.managed()?.clone();
        if m.children.iter().any(|id| id == child) == present {
            return Ok(());
        }
        require(
            m.pending.is_none(),
            "PENDING_OPERATION",
            "Resolve the parent's pending write first",
        )?;
        if present {
            m.children.push(child.into());
        } else {
            m.children.retain(|id| id != child);
        }
        self.store.save(&w.native, &m).await
    }
    /// Validate cross-field references before any write, without requiring readiness in Backlog/Todo.
    fn check_fields(kind: Kind, f: &Value, parent: Option<&str>, graph: &[Work]) -> Result<()> {
        require(
            f["work_type"].is_string(),
            "INVALID_INPUT",
            "work_type cannot be removed",
        )?;
        for value in f.as_object().into_iter().flat_map(|o| o.values()) {
            if let Some(s) = value.as_str() {
                require(
                    !s.lines().any(|line| line.starts_with("## ")),
                    "INVALID_INPUT",
                    "Use level-three or deeper headings inside field values",
                )?;
            }
        }
        if let Some(id) = f["after_epic"].as_str() {
            require(
                kind == Kind::Module && parent.is_none(),
                "INVALID_INPUT",
                "after_epic belongs only to a Project-level Module",
            )?;
            require(
                rules::find(graph, id)
                    .is_some_and(|w| w.meta.as_ref().is_some_and(|m| m.kind == Kind::Epic)),
                "INVALID_INPUT",
                "after_epic must reference an Epic in the same Project",
            )?;
        }
        if f["work_type"] == "integration" {
            require(
                kind == Kind::Atomic,
                "INVALID_INPUT",
                "Only Atomic can be integration work",
            )?;
            require(
                parent
                    .and_then(|id| rules::find(graph, id))
                    .is_none_or(|w| w.meta.as_ref().is_some_and(|m| m.kind == Kind::Epic)),
                "INVALID_INPUT",
                "Integration belongs directly to Project or Epic",
            )?;
        }
        if kind == Kind::Module {
            require(
                f["work_type"] == "code",
                "INVALID_INPUT",
                "Modules are code deliveries with a PR",
            )?;
        }
        Ok(())
    }
    /// Read issue plus complete project graph and require native identity integrity.
    async fn loaded(&self, id: &str) -> Result<(Work, Vec<Work>)> {
        let w = self.store.work(id).await?;
        let m = w.managed()?;
        self.project(&m.project_id).await?;
        let graph = self.store.graph(&m.project_id).await?;
        Ok((w, graph))
    }
    /// Normalize a mutation request for replay, including its public tool identity.
    fn request(name: &str, a: &Value) -> Value {
        json!({"tool":name,"arguments":a})
    }
    /// Continue only the identical prepared write; another request must resolve the uncertainty first.
    async fn resume(&self, w: &Work, request: &Value) -> Result<Option<Value>> {
        let m = w.managed()?;
        if let Some(p) = &m.pending {
            require(
                p.request == *request,
                "PENDING_OPERATION",
                "Retry the pending request with unchanged arguments first",
            )?;
            return self
                .apply_native(w, p.next.clone(), p.input.clone(), &p.before)
                .await
                .map(Some);
        }
        if let Some(last) = &m.last_request
            && last["arguments"]["request_id"] == request["arguments"]["request_id"]
        {
            require(
                *last == *request,
                "REQUEST_CONFLICT",
                "request_id reused with different arguments",
            )?;
            return Ok(Some(json!({"issue":w.native,"replayed":true})));
        }
        Ok(None)
    }
    /// Persist intent before changing native status/content, so cold restarts can resume safely.
    async fn update(
        &self,
        w: &Work,
        mut next: Meta,
        input: Value,
        request: Value,
    ) -> Result<Value> {
        next.last_request = Some(request.clone());
        next.pending = None;
        let mut prepared = w.managed()?.clone();
        prepared.pending = Some(Box::new(Pending {
            request,
            next: next.clone(),
            input: input.clone(),
            before: w.native.clone(),
        }));
        self.store.save(&w.native, &prepared).await?;
        self.apply_native(w, next, input, &w.native).await
    }
    /// Apply a prepared update only from its saved source, or finalize an already applied target.
    /// Duplicate transitions create/check their native relation rather than assigning the reserved state;
    /// legacy pending stateId inputs use the same path. Concurrent manual changes conflict and are never overwritten.
    async fn apply_native(
        &self,
        w: &Work,
        mut next: Meta,
        input: Value,
        before: &Value,
    ) -> Result<Value> {
        require(
            before.is_object(),
            "PENDING_CONFLICT",
            "Pending write lacks its original native snapshot; inspect it before recovery",
        )?;
        let current = self.store.linear.object("QIssue", "issue", w.id()).await?;
        let mut target = before.clone();
        for (key, value) in input.as_object().unwrap() {
            match key.as_str() {
                "stateId" => target["state"]["id"] = value.clone(),
                "parentId" => {
                    target["parent"] = if value.is_null() {
                        Value::Null
                    } else {
                        json!({"id":value})
                    }
                }
                _ => target[key] = value.clone(),
            }
        }
        let already_applied = same_native(&current, &target);
        require(
            already_applied || same_native(&current, before),
            "PENDING_CONFLICT",
            "Native fields changed while a write was pending; preserve the manual edit and resolve the conflict before retrying",
        )?;
        let native = if next.status == Status::Duplicate && input.get("stateId").is_some() {
            self.apply_duplicate(w, &next, already_applied).await?
        } else if already_applied {
            current
        } else {
            self.store
                .linear
                .call("MUpdateIssue", json!({"id":w.id(),"input":input}))
                .await?["issueUpdate"]["issue"]
                .clone()
        };
        require(
            native["state"]["name"] == next.status.name() && same_native(&native, &target),
            "NATIVE_STATE_MISMATCH",
            "Linear did not confirm all requested fields; inspect context and retry the same request",
        )
        .map_err(Fault::uncertain)?;
        if input.get("description").is_some() {
            next.description = native["description"].as_str().unwrap_or("").to_owned();
        }
        next.completed_at = native["completedAt"].as_str().map(str::to_owned);
        if w.managed()?.parent_id != next.parent_id {
            self.register_child(next.parent_id.as_deref(), w.id(), true)
                .await
                .map_err(Fault::uncertain)?;
            self.register_child(w.managed()?.parent_id.as_deref(), w.id(), false)
                .await
                .map_err(Fault::uncertain)?;
        }
        self.store
            .save(&native, &next)
            .await
            .map_err(Fault::uncertain)?;
        Ok(json!({"issue":native,"round":next.round}))
    }
    /// Resolve a stored Linear issue URL without visiting it; reject malformed/external URLs, missing issues and self-links.
    /// `fields` contains the proposed retirement fields, and the returned native Issue supplies the canonical UUID.
    async fn duplicate_target(&self, w: &Work, fields: &Value) -> Result<Value> {
        let url = reqwest::Url::parse(text(fields, "duplicate_of")?)
            .map_err(|_| Fault::new("INVALID_INPUT", "duplicate_of must be a Linear issue URL"))?;
        require(
            url.scheme() == "https"
                && url.host_str() == Some("linear.app")
                && url.username().is_empty()
                && url.password().is_none(),
            "INVALID_INPUT",
            "duplicate_of must be a Linear issue URL",
        )?;
        let segments: Vec<_> = url.path_segments().into_iter().flatten().collect();
        let original = segments
            .windows(2)
            .find(|pair| pair[0] == "issue" && !pair[1].is_empty())
            .map(|pair| pair[1])
            .ok_or_else(|| {
                Fault::new("INVALID_INPUT", "duplicate_of must identify a Linear issue")
            })?;
        let original = self
            .store
            .linear
            .object("QIssue", "issue", original)
            .await?;
        require(
            original["id"] != w.id(),
            "INVALID_INPUT",
            "An issue cannot duplicate itself",
        )?;
        Ok(original)
    }
    /// Apply or confirm a prepared Duplicate transition from `next` without directly setting its reserved status.
    /// Rejects conflicting live duplicate relations and reuses a request-derived relation UUID.
    /// An already-applied status requires the matching relation; response loss remains uncertain,
    /// and the caller verifies the returned native issue before finalizing metadata.
    async fn apply_duplicate(&self, w: &Work, next: &Meta, already_applied: bool) -> Result<Value> {
        let original = self.duplicate_target(w, &next.fields).await?;
        let relations = self
            .store
            .pages("QIssueRelations", "/issue/relations", json!({"id":w.id()}))
            .await?;
        let duplicates: Vec<_> = relations
            .iter()
            .filter(|r| r["type"] == "duplicate" && r["archivedAt"].is_null())
            .collect();
        require(
            duplicates
                .iter()
                .all(|r| r["issue"]["id"] == w.id() && r["relatedIssue"]["id"] == original["id"]),
            "PENDING_CONFLICT",
            "Native duplicate relation points to another issue; preserve it and resolve the conflict before retrying",
        )?;
        require(
            !already_applied || !duplicates.is_empty(),
            "PENDING_CONFLICT",
            "Native Duplicate status lacks the expected original issue relation",
        )?;
        if duplicates.is_empty() {
            let request_id = text(
                &next.last_request.as_ref().unwrap()["arguments"],
                "request_id",
            )?;
            let result = self.store.linear.call("MCreateIssueRelation", json!({"input":{
                "id":child_id(request_id,"duplicate"),"issueId":w.id(),"relatedIssueId":original["id"],"type":"duplicate"
            }})).await?;
            let relation = &result["issueRelationCreate"]["issueRelation"];
            require(
                relation["type"] == "duplicate"
                    && relation["issue"]["id"] == w.id()
                    && relation["relatedIssue"]["id"] == original["id"]
                    && relation["archivedAt"].is_null(),
                "NATIVE_STATE_MISMATCH",
                "Linear did not confirm the requested duplicate relation",
            )
            .map_err(Fault::uncertain)?;
        }
        self.store
            .linear
            .object("QIssue", "issue", w.id())
            .await
            .map_err(Fault::uncertain)
    }
    /// Apply one managed issue edit for `kind`; missing title normalizes the existing title, missing priority preserves it, and priority zero clears it.
    /// Empty or title/priority-only edits preserve native descriptions, stored fields and review identity in any status, without validating or adopting legacy content; requirement/parent edits validate content, invalidate review and require reopening reviewed work, except the existing Module merge-report allowance. Returns the confirmed issue outcome or a safe conflict/write fault.
    /// Explicit repository_path edits validate the local Git checkout before preparing a write.
    async fn edit_work(&self, kind: Kind, a: &Value) -> Result<Value> {
        let (w, graph) = self.loaded(text(a, "id")?).await?;
        let m = w.managed()?;
        require(
            m.kind == kind,
            "WRONG_KIND",
            "Use the edit tool matching this Issue type",
        )?;
        let request = Self::request(&format!("edit_{}", kind.label().to_lowercase()), a);
        if let Some(v) = self.resume(&w, &request).await? {
            return Ok(v);
        }
        let mut errors = rules::discrepancies(&w, &graph);
        errors.retain(|e| !e.starts_with("Description changed"));
        rules::enforce(errors)?;
        let mut next = m.clone();
        let patch = a.get("fields").cloned().unwrap_or(json!({}));
        let presentation_only = patch.as_object().is_some_and(|o| o.is_empty())
            && a.get("parent_id")
                .is_none_or(|v| v.as_str() == m.parent_id.as_deref());
        let mut fields = if w.native["description"] == m.description || presentation_only {
            m.fields.clone()
        } else {
            read_fields(w.native["description"].as_str().unwrap_or(""))?
        };
        let merge_only = kind == Kind::Module
            && patch
                .as_object()
                .is_some_and(|o| !o.is_empty() && o.keys().all(|k| k == "merge_report"))
            && a.get("title").is_none()
            && a.get("priority").is_none()
            && a.get("parent_id").is_none()
            && w.native["description"] == m.description;
        let content_edit = !patch.as_object().unwrap().is_empty()
            || a.get("parent_id")
                .is_some_and(|v| v.as_str() != m.parent_id.as_deref());
        require(
            !matches!(m.status, Status::InReview | Status::Done) || merge_only || !content_edit,
            "REOPEN_REQUIRED",
            "Reopen reviewed work before editing its requirements or result",
        )?;
        if a.get("parent_id").is_some() {
            let parent = a["parent_id"].as_str();
            if parent != m.parent_id.as_deref() {
                require(
                    m.round == 0,
                    "REOPEN_REQUIRED",
                    "Parent changes are only allowed before the first start",
                )?;
                if let Some(old) = m
                    .parent_id
                    .as_deref()
                    .and_then(|id| rules::find(&graph, id))
                {
                    require(
                        !(kind == Kind::Module && old.managed()?.frozen_modules.is_some()),
                        "FROZEN_EPIC",
                        "Cannot detach a Module from a frozen Epic",
                    )?;
                }
                rules::enforce(rules::hierarchy(kind, parent, &graph, Some(w.id())))?;
                if let Some(p) = parent.and_then(|id| rules::find(&graph, id)) {
                    require(
                        p.native["project"]["id"] == m.project_id && !p.status()?.terminal(),
                        "INVALID_PARENT",
                        "Target parent must be open and in the same Project",
                    )?;
                    rules::enforce(rules::discrepancies(p, &graph))?;
                }
                next.parent_id = parent.map(str::to_owned);
            }
        }
        for (k, v) in patch.as_object().unwrap() {
            if v.is_null() {
                fields.as_object_mut().unwrap().remove(k);
            } else {
                fields[k] = v.clone();
            }
        }
        if !presentation_only {
            self.catalog.validate_fields(kind, &fields)?;
            Self::check_fields(kind, &fields, next.parent_id.as_deref(), &graph)?;
        }
        if let Some(path) = patch["repository_path"].as_str() {
            crate::git::validate_repository(path)?;
        }
        next.fields = fields;
        if !presentation_only {
            next.description =
                patch_description(w.native["description"].as_str().unwrap_or(""), &patch);
        }
        if content_edit && !merge_only {
            next.revision += 1;
            next.review = None;
        }
        let mut input = json!({});
        if let Some(title) = a.get("title") {
            input["title"] = json!(kind.title(title.as_str().unwrap_or(""))?);
        } else {
            let current = w.native["title"].as_str().unwrap_or("");
            input["title"] = json!(kind.title(current)?);
        }
        if let Some(priority) = a.get("priority") {
            input["priority"] = priority.clone();
        }
        if content_edit {
            input["description"] = json!(next.description.clone());
        }
        if a.get("parent_id").is_some() {
            input["parentId"] = a["parent_id"].clone();
        }
        require(
            !input.as_object().unwrap().is_empty(),
            "INVALID_INPUT",
            "No issue fields to edit",
        )?;
        self.update(&w, next, input, request).await
    }
    /// Publish each persisted current-round Git report once through the normal comment writer.
    /// Deterministic native IDs make lost comment replies recoverable without rereading Git.
    async fn ensure_commit_comments(
        &self,
        work_id: &str,
        reports: &[crate::model::LocalGitReport],
    ) -> Result<Vec<Value>> {
        let mut comments = Vec::with_capacity(reports.len());
        for report in reports {
            let commit = &report.commit;
            let id = child_id(
                work_id,
                &format!(
                    "git-report:{}:{}:{}",
                    report.round, commit.repository_identity, commit.sha
                ),
            );
            let mut body = format!(
                "{}\nCommit: {}\n\n### Result\n{}\n\n### Checks reported by author\n{}",
                commit.subject, commit.sha, commit.result, commit.checks
            );
            if let Some(notes) = &commit.notes {
                body.push_str(&format!("\n\n### Notes\n{notes}"));
            }
            let actor = commit.author.replace(['\n', '\r'], " ");
            let value = self
                .add_comment(&json!({
                    "request_id":id,"actor":actor,"target_type":"issue","target_id":work_id,
                    "kind":"progress","role":"git author","body":body
                }))
                .await?;
            comments.push(value["comment"].clone());
        }
        Ok(comments)
    }

    /// Import concrete commits for an active code Task/Atomic using its assigned checkout.
    /// Resumes the exact pending request before reading Git. Validates every source before writing,
    /// deduplicates current-round repository/SHA pairs, preserves history and fills result/checks.
    /// Journals persisted reports through native comments, then returns reports and permalinks
    /// without changing status; uncertain native outcomes retry from the same snapshot.
    async fn record_commits(&self, a: &Value) -> Result<Value> {
        let (w, graph) = self.loaded(text(a, "work_id")?).await?;
        let request = Self::request("record_commits", a);
        if let Some(mut outcome) = self.resume(&w, &request).await? {
            let restored = self.store.work(w.id()).await.map_err(Fault::uncertain)?;
            let reports: Vec<_> = restored.managed()?.current_git_reports().cloned().collect();
            outcome["journal"] = json!(self.ensure_commit_comments(w.id(), &reports).await?);
            outcome["git_reports"] = json!(reports);
            return Ok(outcome);
        }
        let m = w.managed()?;
        require(
            matches!(m.kind, Kind::Task | Kind::Atomic) && w.fields["work_type"] == "code",
            "WRONG_KIND",
            "Commit reports belong to code Tasks or Atomics",
        )?;
        require(
            w.status()? == Status::InProgress,
            "REOPEN_REQUIRED",
            "Start or reopen this work before importing commits",
        )?;
        rules::enforce(rules::discrepancies(&w, &graph))?;
        let owner = rules::parent(&w)
            .and_then(|id| rules::find(&graph, id))
            .filter(|p| p.meta.as_ref().is_some_and(|m| m.kind == Kind::Module))
            .unwrap_or(&w);
        rules::enforce(rules::discrepancies(owner, &graph))?;
        let path = text(&owner.fields, "worktree")?;
        let expected_repository = owner.fields["repository_path"]
            .as_str()
            .map(crate::git::repository_identity)
            .transpose()?;
        let mut next = m.clone();
        for hash in a["commits"].as_array().unwrap() {
            let commit = crate::git::read_commit(path, hash.as_str().unwrap())?;
            require(
                expected_repository
                    .as_ref()
                    .is_none_or(|id| *id == commit.repository_identity),
                "REPOSITORY_MISMATCH",
                "Assigned worktree belongs to a different repository",
            )?;
            if !next.current_git_reports().any(|r| {
                r.commit.repository_identity == commit.repository_identity
                    && r.commit.sha == commit.sha
            }) {
                next.git_reports.push(crate::model::LocalGitReport {
                    round: m.round,
                    commit,
                });
            }
        }
        let reports: Vec<_> = next.current_git_reports().collect();
        let patch = json!({
            "result":reports.iter().map(|r| format!("### {} {}\n\n{}", &r.commit.sha[..12], r.commit.subject, r.commit.result)).collect::<Vec<_>>().join("\n\n"),
            "check_result":reports.iter().map(|r| format!("### {}\n\n{}", &r.commit.sha[..12], r.commit.checks)).collect::<Vec<_>>().join("\n\n")
        });
        let patch = patch
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, value)| {
                // Native level-two headings delimit workflow fields, so source headings render deeper.
                let body = value
                    .as_str()
                    .unwrap()
                    .lines()
                    .map(|line| {
                        if line.starts_with("## ") {
                            format!("#{line}")
                        } else {
                            line.to_owned()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                (key.clone(), json!(body))
            })
            .collect::<serde_json::Map<_, _>>();
        let patch = Value::Object(patch);
        self.catalog.validate_fields(m.kind, &patch)?;
        let changed = next.git_reports.len() != m.git_reports.len()
            || patch
                .as_object()
                .unwrap()
                .iter()
                .any(|(key, value)| m.fields[key] != *value);
        for (key, value) in patch.as_object().unwrap() {
            next.fields[key] = value.clone();
        }
        next.description =
            patch_description(w.native["description"].as_str().unwrap_or(""), &patch);
        if changed {
            next.revision += 1;
            next.review = None;
        }
        let reports: Vec<_> = next.current_git_reports().cloned().collect();
        let input = json!({"description":next.description});
        let mut outcome = self.update(&w, next, input, request).await?;
        outcome["journal"] = json!(self.ensure_commit_comments(w.id(), &reports).await?);
        outcome["git_reports"] = json!(reports);
        Ok(outcome)
    }

    /// Guard and explicitly move work; check_only never persists intent, labels or changes.
    /// Module submission stores the same derived current-child result used by context/readiness,
    /// preserving a real PR requirement and binding review to the resulting content revision.
    async fn move_status(&self, a: &Value) -> Result<Value> {
        let (w, graph) = self.loaded(text(a, "id")?).await?;
        let target: Status = serde_json::from_value(a["status"].clone()).unwrap();
        let request = Self::request("move_status", a);
        if a["check_only"] != true
            && let Some(v) = self.resume(&w, &request).await?
        {
            return Ok(v);
        }
        let errors = rules::transition(&w, &graph, target, text(a, "actor_role")?);
        if a["check_only"] == true {
            return Ok(json!({"allowed":errors.is_empty(),"conditions":errors,"status":target}));
        }
        rules::enforce(errors)?;
        let m = w.managed()?;
        if w.status()? == target && m.status == target && !rules::restart_integration(&w, &graph) {
            return Ok(json!({"issue":w.native,"unchanged":true}));
        }
        if target == Status::Duplicate {
            self.duplicate_target(&w, &m.fields).await?;
        }
        let states = self
            .states(w.native["team"]["id"].as_str().unwrap())
            .await?;
        let state = Self::state_id(&states, target)?;
        let mut next = m.clone();
        next.status = target;
        let mut input = json!({"stateId":state});
        if target == Status::InReview && m.kind == Kind::Module {
            let report = crate::reports::module_report(&w, &graph)?;
            let patch = json!({"result":report.summary,"check_result":report.reported_checks});
            self.catalog.validate_fields(Kind::Module, &patch)?;
            if next.fields["result"] != patch["result"]
                || next.fields["check_result"] != patch["check_result"]
            {
                next.fields["result"] = patch["result"].clone();
                next.fields["check_result"] = patch["check_result"].clone();
                next.description =
                    patch_description(w.native["description"].as_str().unwrap_or(""), &patch);
                next.revision += 1;
                next.review = None;
                input["description"] = json!(next.description);
            }
        }
        if target == Status::InProgress {
            next.round += 1;
            next.review = None;
            next.completed_at = None;
            if next.kind == Kind::Epic && next.frozen_modules.is_none() {
                next.frozen_modules = Some(
                    rules::children(&graph, w.id())
                        .iter()
                        .filter(|v| v.meta.as_ref().is_some_and(|m| m.kind == Kind::Module))
                        .map(|v| v.id().to_string())
                        .collect(),
                );
            }
            if m.round > 0 {
                let mut remove = json!({"result":null,"check_result":null,"commit_url":null,"artifact_url":null,"merge_report":null});
                if m.kind == Kind::Module && m.status == Status::Done {
                    remove["pr_url"] = Value::Null;
                }
                for k in remove.as_object().unwrap().keys() {
                    next.fields.as_object_mut().unwrap().remove(k);
                }
                next.description = patch_description(&m.description, &remove);
                input["description"] = json!(next.description);
                next.revision += 1;
            }
            if next.fields["work_type"] == "integration" {
                next.integration = rules::module_ids(&next.fields)
                    .iter()
                    .filter_map(|id| {
                        rules::find(&graph, id).map(|w| (id.clone(), rules::completion(w)))
                    })
                    .collect();
            }
        }
        self.update(&w, next, input, request).await
    }

    /// Create a comment with caller-allocated native ID, checking target and reply parent first,
    /// then confirming returned target, parent and normalized body. Identical replay returns its permalink.
    async fn add_comment(&self, a: &Value) -> Result<Value> {
        let id = text(a, "request_id")?;
        let target_id = text(a, "target_id")?;
        let target_type = text(a, "target_type")?;
        match target_type {
            "issue" => {
                self.store
                    .linear
                    .object("QIssue", "issue", target_id)
                    .await?;
            }
            "project" => {
                self.project(target_id).await?;
            }
            _ => {
                self.store
                    .linear
                    .object("QProjectUpdate", "projectUpdate", target_id)
                    .await?;
            }
        }
        if let Some(parent_id) = a["parent_id"].as_str() {
            let parent = self
                .store
                .linear
                .object("QComment", "comment", parent_id)
                .await?;
            require(
                parent["parent"].is_null()
                    && crate::activity::target(&parent)? == (target_type, target_id),
                "INVALID_PARENT",
                "Reply parent must be a root comment on the same target",
            )?;
        }
        let kind = a["kind"].as_str().unwrap_or("note");
        require(
            kind != "question"
                || a["recipient"]
                    .as_str()
                    .is_some_and(|v| !v.trim().is_empty()),
            "INVALID_INPUT",
            "Questions require a recipient",
        )?;
        let body = crate::activity::render(
            kind,
            a["role"].as_str().unwrap_or("participant"),
            text(a, "actor")?,
            text(a, "body")?,
            a,
        )?;
        if let Some(comment) = self.store.optional("QComment", "comment", id).await? {
            require(
                crate::activity::target(&comment)? == (target_type, target_id)
                    && comment["parent"]["id"] == a["parent_id"]
                    && markdown_key(comment["body"].as_str().unwrap_or("")) == markdown_key(&body),
                "REQUEST_CONFLICT",
                "Comment request_id already names different content",
            )?;
            return Ok(json!({"comment":comment,"replayed":true}));
        }
        let mut input = json!({"id":id,"body":body});
        let key = match target_type {
            "issue" => "issueId",
            "project" => "projectId",
            _ => "projectUpdateId",
        };
        input[key] = json!(target_id);
        if let Some(parent_id) = a["parent_id"].as_str() {
            input["parentId"] = json!(parent_id);
        }
        let comment = self
            .store
            .linear
            .call("MCreateComment", json!({"input":input}))
            .await?["commentCreate"]["comment"]
            .clone();
        require(
            crate::activity::target(&comment).map_err(Fault::uncertain)?
                == (target_type, target_id)
                && comment["parent"]["id"] == a["parent_id"]
                && markdown_key(comment["body"].as_str().unwrap_or("")) == markdown_key(&body),
            "NATIVE_STATE_MISMATCH",
            "Linear did not confirm comment content and ownership",
        )
        .map_err(Fault::uncertain)?;
        Ok(json!({"comment":comment,"replayed":false}))
    }

    /// Read a comment by UUID or exact native permalink. Resolve ProjectUpdate short tokens
    /// within the URL's Project before matching the full returned URL and reading one reply page.
    /// ponytail: unscoped links scan at most 20,000 comments; add a native hash lookup if that ceiling matters.
    async fn get_comment(&self, a: &Value) -> Result<Value> {
        let supplied = text(a, "id")?;
        let id = if uuid::Uuid::parse_str(supplied).is_ok() {
            supplied.to_owned()
        } else {
            let url = reqwest::Url::parse(supplied)
                .map_err(|_| Fault::new("INVALID_LINK", "Expected a native Linear comment URL"))?;
            let fragment = url.fragment().unwrap_or("");
            let (update_short, comment_part) =
                if let Some((prefix, suffix)) = fragment.split_once('&') {
                    (prefix.strip_prefix("project-update-"), suffix)
                } else {
                    (None, fragment)
                };
            let hash = comment_part.strip_prefix("comment-").unwrap_or("");
            require(
                url.scheme() == "https"
                    && url.host_str() == Some("linear.app")
                    && (!fragment.contains('&')
                        || update_short.is_some_and(|id| {
                            id.len() == 8 && id.bytes().all(|b| b.is_ascii_hexdigit())
                        }))
                    && hash.len() == 8
                    && hash.bytes().all(|b| b.is_ascii_hexdigit()),
                "INVALID_LINK",
                "Expected an observed Linear comment permalink",
            )?;
            let segments: Vec<_> = url.path_segments().map(|s| s.collect()).unwrap_or_default();
            let mut filter = json!({});
            if let Some(update_short) = update_short {
                let project_slug = segments
                    .windows(2)
                    .find_map(|pair| (pair[0] == "project").then_some(pair[1]))
                    .ok_or_else(|| {
                        Fault::new("INVALID_LINK", "ProjectUpdate link has no Project")
                    })?;
                let project = self
                    .store
                    .linear
                    .object("QProject", "project", project_slug)
                    .await?;
                let updates = self.store.pages(
                    "QProjectUpdates", "projectUpdates",
                    json!({"filter":{"project":{"id":{"eq":project["id"]}}},"includeArchived":true}),
                ).await?;
                let matches: Vec<_> = updates
                    .iter()
                    .filter(|update| {
                        update["id"]
                            .as_str()
                            .and_then(|id| id.get(..8))
                            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(update_short))
                    })
                    .collect();
                require(
                    !matches.is_empty(),
                    "RECORD_MISSING",
                    "ProjectUpdate link target was not found",
                )?;
                require(
                    matches.len() == 1,
                    "INVALID_LINK",
                    "ProjectUpdate short token is ambiguous",
                )?;
                filter["projectUpdate"] = json!({"id":{"eq":matches[0]["id"]}});
            } else if let Some(pos) = segments.iter().position(|s| *s == "issue")
                && let Some(identifier) = segments.get(pos + 1)
            {
                let issue = self
                    .store
                    .linear
                    .object("QIssue", "issue", identifier)
                    .await?;
                filter["issue"] = json!({"id":{"eq":issue["id"]}});
            }
            let comments = self
                .store
                .pages(
                    "QComments",
                    "comments",
                    json!({"filter":filter,"includeArchived":true}),
                )
                .await?;
            comments
                .into_iter()
                .find(|c| c["url"] == supplied)
                .and_then(|c| c["id"].as_str().map(str::to_owned))
                .ok_or_else(|| {
                    Fault::new("RECORD_MISSING", "Native comment permalink was not found")
                })?
        };
        let comment = self.store.linear.object("QComment", "comment", &id).await?;
        let root_id = comment["parent"]["id"].as_str().unwrap_or(&id);
        let root = if root_id == id {
            comment.clone()
        } else {
            self.store
                .linear
                .object("QComment", "comment", root_id)
                .await?
        };
        let replies = self
            .store
            .linear
            .call(
                "QCommentChildren",
                json!({
                    "id":root_id,
                    "first":a.get("first").and_then(Value::as_u64).unwrap_or(50),
                    "after":a.get("after").unwrap_or(&Value::Null)
                }),
            )
            .await?["comment"]["children"]
            .clone();
        require(
            replies["nodes"].is_array(),
            "INCOMPLETE_DATA",
            "Comment thread page is missing",
        )?;
        let (kind, target_id) = crate::activity::target(&comment)?;
        let current = if kind == "issue" {
            self.store.meta(target_id).await?.and_then(|m| m.review)
        } else {
            None
        };
        let activity = crate::activity::record(&comment, current.as_ref())?;
        Ok(json!({"comment":comment,"activity":activity,"root":root,"replies":replies}))
    }

    /// Resolve or reopen a native top-level thread; checking current state makes retry safe.
    async fn resolve_comment(&self, a: &Value) -> Result<Value> {
        let id = text(a, "id")?;
        let comment = self.store.linear.object("QComment", "comment", id).await?;
        require(
            comment["parent"].is_null(),
            "INVALID_PARENT",
            "Resolve the root comment",
        )?;
        if let Some(reply_id) = a["resolving_comment_id"].as_str() {
            require(
                a["resolved"] == true,
                "INVALID_INPUT",
                "A resolving reply requires resolved=true",
            )?;
            let reply = self
                .store
                .linear
                .object("QComment", "comment", reply_id)
                .await?;
            require(
                reply["parent"]["id"] == id
                    && crate::activity::target(&reply)? == crate::activity::target(&comment)?,
                "INVALID_PARENT",
                "Resolving reply must belong to this thread",
            )?;
        }
        let resolved = a["resolved"] == true;
        if comment["resolvedAt"].is_string() == resolved {
            require(
                !resolved
                    || a["resolving_comment_id"].is_null()
                    || comment["resolvingCommentId"] == a["resolving_comment_id"],
                "REQUEST_CONFLICT",
                "Thread was resolved with another reply",
            )?;
            return Ok(json!({"comment":comment,"replayed":true}));
        }
        let (operation, variables, field) = if resolved {
            (
                "MResolveComment",
                json!({"id":id,"resolvingCommentId":a["resolving_comment_id"]}),
                "commentResolve",
            )
        } else {
            ("MUnresolveComment", json!({"id":id}), "commentUnresolve")
        };
        let changed = self.store.linear.call(operation, variables).await?[field]["comment"].clone();
        require(
            changed["resolvedAt"].is_string() == resolved,
            "NATIVE_STATE_MISMATCH",
            "Linear did not confirm thread resolution",
        )
        .map_err(Fault::uncertain)?;
        Ok(json!({"comment":changed,"replayed":false}))
    }

    /// Persist a native review activity comment and current-round decision, returning its
    /// permalink without implicitly transitioning work or adding Task review.
    async fn review(&self, a: &Value) -> Result<Value> {
        let (w, graph) = self.loaded(text(a, "id")?).await?;
        let m = w.managed()?;
        let request = Self::request("record_review", a);
        if self.resume(&w, &request).await?.is_some() {
            let restored = self.store.work(w.id()).await.map_err(Fault::uncertain)?;
            let comment = self
                .store
                .linear
                .object("QComment", "comment", text(a, "request_id")?)
                .await
                .map_err(Fault::uncertain)?;
            return Ok(
                json!({"review":restored.managed()?.review,"issue_id":w.id(),"comment":comment,"url":comment["url"],"replayed":true}),
            );
        }
        rules::enforce(rules::discrepancies(&w, &graph))?;
        require(
            m.kind != Kind::Task,
            "NO_TASK_REVIEW",
            "Review the entire Module instead",
        )?;
        require(
            w.status()? == Status::InReview,
            "NOT_IN_REVIEW",
            "Move this work to In Review first",
        )?;
        let id = text(a, "request_id")?;
        let content = format!(
            "{}\n\n### Findings\n{}",
            text(a, "summary")?,
            a["findings"]
                .as_str()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or("None")
        );
        let body = crate::activity::render(
            "review",
            "reviewer",
            text(a, "actor")?,
            &content,
            &json!({"reviewer":a["reviewer"],"session":a["session"],"verdict":a["verdict"],
                "round":m.round,"revision":m.revision,"source_links":a["artifacts"]}),
        )?;
        if let Some(comment) = self.store.optional("QComment", "comment", id).await? {
            require(
                comment["issue"]["id"] == w.id()
                    && markdown_key(comment["body"].as_str().unwrap_or("")) == markdown_key(&body),
                "REQUEST_CONFLICT",
                "Review request_id already names another report",
            )?;
        } else {
            self.store
                .linear
                .call(
                    "MCreateComment",
                    json!({"input":{"id":id,"issueId":w.id(),"body":body}}),
                )
                .await?;
        }
        let comment = self
            .store
            .linear
            .object("QComment", "comment", id)
            .await
            .map_err(Fault::uncertain)?;
        let mut next = m.clone();
        next.review = Some(Review {
            id: id.into(),
            round: m.round,
            revision: m.revision,
            accepted: a["verdict"] == "accepted",
        });
        next.last_request = Some(request);
        self.store
            .save(&w.native, &next)
            .await
            .map_err(Fault::uncertain)?;
        Ok(
            json!({"review":next.review,"issue_id":w.id(),"comment":comment,"url":comment["url"],"replayed":false}),
        )
    }
    /// Load one complete Project graph and bounded native activity, then compose a read-only view.
    /// The limit prevents a large Project from turning a single overview into unbounded API reads.
    async fn overview_data(&self, project_id: &str) -> Result<(Value, BTreeMap<String, Value>)> {
        let project = self.project(project_id).await?;
        let graph = self.store.graph(project_id).await?;
        require(
            graph.len() <= 300,
            "INCOMPLETE_DATA",
            "Overview exceeds 300 work items",
        )?;
        let updates = self
            .store
            .pages(
                "QProjectUpdates",
                "projectUpdates",
                json!({"filter":{"project":{"id":{"eq":project_id}}},"includeArchived":false}),
            )
            .await?;
        require(
            updates.len() <= 100,
            "INCOMPLETE_DATA",
            "Overview exceeds 100 ProjectUpdates",
        )?;
        let mut activity = BTreeMap::new();
        activity.insert(
            project_id.to_owned(),
            crate::activity::read_activity(&self.store, "project", project_id).await?,
        );
        for work in &graph {
            if work.meta.is_some() {
                activity.insert(
                    work.id().to_owned(),
                    crate::activity::read_activity(&self.store, "issue", work.id()).await?,
                );
            }
        }
        for update in &updates {
            let id = update["id"]
                .as_str()
                .ok_or_else(|| Fault::new("INCOMPLETE_DATA", "ProjectUpdate has no ID"))?;
            let mut records =
                crate::activity::read_activity(&self.store, "project_update", id).await?;
            records.push(crate::activity::project_update_record(update)?);
            activity.insert(id.to_owned(), records);
        }
        let overview = crate::context::project_overview(&project, &graph, &activity)?;
        let snapshot = crate::context::compact_snapshot(&project, &graph, &activity)?;
        Ok((overview, snapshot))
    }

    /// Return a full overview or a same-Project delta and a fresh opaque comparison point.
    /// Missing process-local baselines fall back to a full response with baseline_expired=true;
    /// neither branch writes to Linear or launches background activity.
    async fn overview(&self, a: &Value) -> Result<Value> {
        let project_id = text(a, "project_id")?;
        let (mut full, snapshot) = self.overview_data(project_id).await?;
        let comparison = self
            .baselines
            .lock()
            .map_err(|_| {
                Fault::new(
                    "INCOMPLETE_DATA",
                    "Overview comparison cache is unavailable",
                )
            })?
            .compare(project_id, a["cursor"].as_str(), snapshot, Instant::now());
        let observed_at = chrono::Utc::now().to_rfc3339();
        if comparison["changes"].is_array() {
            Ok(json!({"project_id":project_id,"observed_at":observed_at,
                "cursor":comparison["cursor"],"baseline_expired":false,
                "changes":comparison["changes"],"project_update_draft":full["project_update_draft"]}))
        } else {
            full["observed_at"] = json!(observed_at);
            full["cursor"] = comparison["cursor"].clone();
            full["baseline_expired"] = comparison["baseline_expired"].clone();
            Ok(full)
        }
    }

    /// Read native work by UUID or a native Issue link. Legacy type/ID calls keep their original
    /// response; optional lead/reviewer views add a bounded assignment and evidence projection.
    /// Derived counts are withheld when native membership differs from the recorded graph.
    async fn context(&self, a: &Value) -> Result<Value> {
        require(
            !(a["id"].is_string() && a["url"].is_string()),
            "INVALID_INPUT",
            "Supply id or url, not both",
        )?;
        let reference = a["url"]
            .as_str()
            .or_else(|| a["id"].as_str())
            .ok_or_else(|| Fault::new("INVALID_INPUT", "Supply id or url"))?;
        let linked = reference.starts_with("https://") || a["url"].is_string();
        let kind = if linked {
            require(
                a["type"].is_null() || a["type"] == "issue",
                "INVALID_LINK",
                "Issue URLs require type issue",
            )?;
            "issue"
        } else {
            text(a, "type")?
        };
        let resolved;
        let id = if linked {
            let identifier = crate::context::issue_identifier(reference)?;
            let issue = self
                .store
                .linear
                .object("QIssue", "issue", &identifier)
                .await?;
            require(
                issue["identifier"] == identifier,
                "INVALID_LINK",
                "Issue URL resolved to another item",
            )?;
            resolved = issue["id"].as_str().unwrap_or("").to_owned();
            resolved.as_str()
        } else {
            reference
        };
        match kind {
            "project" => {
                let p = self.project(id).await?;
                let docs = self
                    .store
                    .pages(
                        "QDocuments",
                        "documents",
                        json!({"filter":{"project":{"id":{"eq":id}}},"includeArchived":false}),
                    )
                    .await?;
                Ok(json!({"project":p,"documents":docs}))
            }
            "document" => self.store.linear.object("QDocument", "document", id).await,
            "project_update" => {
                let update = self
                    .store
                    .linear
                    .object("QProjectUpdate", "projectUpdate", id)
                    .await?;
                let activity = crate::activity::project_update_record(&update)?;
                Ok(json!({"project_update":update,"activity":activity}))
            }
            _ => {
                let (w, g) = self.loaded(id).await?;
                let checkout=rules::parent(&w).and_then(|p|rules::find(&g,p)).filter(|p|p.meta.as_ref().is_some_and(|m|m.kind==Kind::Module)).map(|p|json!({"repository_path":p.fields["repository_path"],"repository_url":p.fields["repository_url"],"branch":p.fields["branch"],"worktree":p.fields["worktree"],"lead":p.fields["lead"]}));
                let m = w.managed()?;
                let discrepancies = rules::discrepancies(&w, &g);
                let module_report = if m.kind == Kind::Module && discrepancies.is_empty() {
                    Some(crate::reports::module_report(&w, &g)?)
                } else {
                    None
                };
                let agent_view = if let Some(view) = a["view"].as_str().or(linked.then_some("lead"))
                {
                    let children = rules::children(&g, w.id());
                    require(
                        children.len() <= 200,
                        "INCOMPLETE_DATA",
                        "Agent context has more than 200 direct children",
                    )?;
                    let mut activity = BTreeMap::new();
                    for target in std::iter::once(&w).chain(children) {
                        activity.insert(
                            target.id().to_owned(),
                            crate::activity::read_activity(&self.store, "issue", target.id())
                                .await?,
                        );
                    }
                    let documents = self.store.pages("QDocuments", "documents", json!({"filter":{"project":{"id":{"eq":m.project_id}}},"includeArchived":false})).await?;
                    Some(crate::context::agent_context(
                        &w,
                        &g,
                        view,
                        &activity,
                        &documents,
                        module_report.as_ref(),
                    )?)
                } else {
                    None
                };
                let peers: Vec<_> = g
                    .iter()
                    .filter(|peer| {
                        peer.id() != id
                            && peer.native["labels"]["nodes"]
                                .as_array()
                                .is_some_and(|labels| {
                                    labels.iter().any(|label| label["name"] == m.kind.label())
                                })
                            && peer.native["project"]["id"] == m.project_id
                            && peer.native["parent"]["id"].as_str() == m.parent_id.as_deref()
                    })
                    .collect();
                let mut peers = peers;
                peers.sort_by(|a, b| priority_cmp(&a.native, &b.native));
                let priority_group = json!({"project_id":m.project_id,"parent_id":m.parent_id,"kind":m.kind,"peers":peers.iter().map(|p|json!({"id":p.id(),"identifier":p.native["identifier"],"title":p.native["title"],"status":p.native["state"]["name"],"priority":p.native["priority"],"priorityLabel":p.native["priorityLabel"]})).collect::<Vec<_>>()});
                Ok(
                    json!({"issue":w.native,"fields":w.fields,"git_reports":m.current_git_reports().collect::<Vec<_>>(),"module_report":module_report,"workflow":w.meta,"parent_checkout":checkout,"children":rules::children(&g,id).iter().map(|c|&c.native).collect::<Vec<_>>(),"priority_group":priority_group,"discrepancies":discrepancies,"transitions":rules::actions(&w,&g),"agent_context":agent_view}),
                )
            }
        }
    }

    /// List native comments for one optional target and parent with untouched cursor semantics.
    /// A missing target lists workspace comments; unsupported Issue filters are rejected.
    async fn list_comments(&self, a: &Value) -> Result<Value> {
        require(
            a["target_type"].is_string() == a["target_id"].is_string(),
            "INVALID_INPUT",
            "Comment target_type and target_id must be supplied together",
        )?;
        require(
            [
                "kind",
                "status",
                "priority",
                "team_id",
                "project_id",
                "order_by",
            ]
            .iter()
            .all(|key| a.get(*key).is_none()),
            "INVALID_INPUT",
            "Issue filters do not apply to comments",
        )?;
        let mut filter = json!({});
        if let Some(target_type) = a["target_type"].as_str() {
            let key = match target_type {
                "issue" => "issue",
                "project" => "project",
                _ => "projectUpdate",
            };
            filter[key] = json!({"id":{"eq":a["target_id"]}});
        }
        if let Some(parent) = a.get("parent_id") {
            filter["parent"] = if parent.is_null() {
                json!({"null":true})
            } else {
                json!({"id":{"eq":parent}})
            };
        }
        let mut page = self.store.linear.call("QComments", json!({
            "filter":filter,
            "first":a.get("first").and_then(Value::as_u64).unwrap_or(50),
            "after":a.get("after").unwrap_or(&Value::Null),
            "includeArchived":a.get("include_archived").and_then(Value::as_bool).unwrap_or(false)
        })).await?["comments"].clone();
        require(
            page["nodes"].is_array(),
            "INCOMPLETE_DATA",
            "Comment page is missing",
        )?;
        let current = if a["target_type"] == "issue" {
            self.store
                .meta(text(a, "target_id")?)
                .await?
                .and_then(|m| m.review)
        } else {
            None
        };
        page["activity_records"] = json!(
            page["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|comment| crate::activity::record(comment, current.as_ref()))
                .collect::<Result<Vec<_>>>()?
        );
        Ok(page)
    }

    /// List or search one entity type with native cursors. Comments use their target/parent filters;
    /// Issue priority order loads its complete bounded group, sorts, then slices with a scoped cursor.
    async fn list(&self, a: &Value, search: bool) -> Result<Value> {
        let kind = text(a, "type")?;
        if kind == "comment" {
            require(!search, "INVALID_INPUT", "Comment search is unavailable")?;
            return self.list_comments(a).await;
        }
        if kind == "project_update" {
            require(
                !search,
                "INVALID_INPUT",
                "ProjectUpdate search is unavailable",
            )?;
            require(
                [
                    "parent_id",
                    "target_type",
                    "target_id",
                    "team_id",
                    "kind",
                    "status",
                    "priority",
                    "order_by",
                ]
                .iter()
                .all(|key| a.get(*key).is_none()),
                "INVALID_INPUT",
                "Issue and Comment filters do not apply to ProjectUpdates",
            )?;
            let mut filter = json!({});
            if let Some(project_id) = a["project_id"].as_str() {
                filter["project"] = json!({"id":{"eq":project_id}});
            }
            let mut page = self.store.linear.call("QProjectUpdates",json!({
                "filter":filter,
                "first":a.get("first").and_then(Value::as_u64).unwrap_or(50),
                "after":a.get("after").unwrap_or(&Value::Null),
                "includeArchived":a.get("include_archived").and_then(Value::as_bool).unwrap_or(false)
            })).await?["projectUpdates"].clone();
            require(
                page["nodes"].is_array(),
                "INCOMPLETE_DATA",
                "ProjectUpdate page is missing",
            )?;
            page["activity_records"] = json!(
                page["nodes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(crate::activity::project_update_record)
                    .collect::<Result<Vec<_>>>()?
            );
            return Ok(page);
        }
        let (query, field) = match (search, kind) {
            (true, "issue") => ("QSearchIssues", "searchIssues"),
            (true, "project") => ("QSearchProjects", "searchProjects"),
            (true, _) => ("QSearchDocuments", "searchDocuments"),
            (false, "issue") => ("QIssues", "issues"),
            (false, "project") => ("QProjects", "projects"),
            _ => ("QDocuments", "documents"),
        };
        let mut args = json!({"first":a.get("first").unwrap_or(&json!(50)),"after":a.get("after").unwrap_or(&Value::Null)});
        if search {
            args["term"] = a["query"].clone();
        } else {
            args["includeArchived"] = a.get("include_archived").cloned().unwrap_or(json!(false));
            let mut filter = json!({});
            for (key, field) in [
                ("project_id", "project"),
                ("parent_id", "parent"),
                ("team_id", "team"),
            ] {
                if let Some(id) = a.get(key) {
                    require(
                        kind == "issue" || (kind == "document" && key == "project_id"),
                        "INVALID_INPUT",
                        "This filter is not supported for the requested entity type",
                    )?;
                    filter[field] = if key == "parent_id" && id.is_null() {
                        json!({"null":true})
                    } else {
                        json!({"id":{"eq":id}})
                    };
                }
            }
            if let Some(v) = a.get("kind") {
                require(
                    kind == "issue",
                    "INVALID_INPUT",
                    "kind applies only to issues",
                )?;
                let k: Kind = serde_json::from_value(v.clone()).unwrap();
                filter["labels"] = json!({"some":{"name":{"eq":k.label()}}});
            }
            if let Some(v) = a.get("status") {
                require(
                    kind == "issue",
                    "INVALID_INPUT",
                    "status applies only to issues",
                )?;
                filter["state"] = json!({"name":{"eq":v}});
            }
            if let Some(v) = a.get("priority") {
                require(
                    kind == "issue",
                    "INVALID_INPUT",
                    "priority applies only to issues",
                )?;
                filter["priority"] = json!({"eq":v});
            }
            args["filter"] = filter;
        }
        if !search && a["order_by"] == "priority" {
            require(
                kind == "issue" && a["project_id"].is_string() && a["kind"].is_string(),
                "INVALID_INPUT",
                "Priority ordering requires issue type, project_id and kind",
            )?;
            let project = a["project_id"].as_str().unwrap();
            let k: Kind = serde_json::from_value(a["kind"].clone()).unwrap();
            let parent = a["parent_id"].as_str();
            let group = json!({"project_id":project,"parent_id":parent,"kind":k,"team_id":a.get("team_id"),"status":a.get("status"),"include_archived":a.get("include_archived").and_then(Value::as_bool).unwrap_or(false),"priority":a.get("priority")});
            if let Some(pid) = parent {
                let graph = self.store.graph(project).await?;
                let p = rules::find(&graph, pid).ok_or_else(|| {
                    Fault::new("INVALID_PARENT", "Parent must belong to this Project")
                })?;
                require(
                    p.native["project"]["id"] == project
                        && p.meta.as_ref().is_some_and(|pm| pm.project_id == project),
                    "INVALID_PARENT",
                    "Parent must belong to this Project",
                )?;
                let pk = p.managed()?.kind;
                let valid = matches!(
                    (k, pk),
                    (Kind::Module, Kind::Epic)
                        | (Kind::Task, Kind::Module)
                        | (Kind::Atomic, Kind::Epic | Kind::Module)
                );
                require(
                    valid,
                    "INVALID_PARENT",
                    "Parent kind is incompatible with requested kind",
                )?;
            }
            let mut fetch = json!({"filter":{"project":{"id":{"eq":project}}},"includeArchived":group["include_archived"]});
            for (key, field) in [("team_id", "team"), ("status", "state")] {
                if let Some(v) = a.get(key) {
                    fetch["filter"][field] = if key == "team_id" {
                        json!({"id":{"eq":v}})
                    } else {
                        json!({"name":{"eq":v}})
                    };
                }
            }
            fetch["filter"]["labels"] = json!({"some":{"name":{"eq":k.label()}}});
            let mut nodes = self.store.pages("QIssues", "issues", fetch).await?;
            nodes.retain(|v| v["project"]["id"] == project && v["parent"]["id"].as_str() == parent);
            if let Some(priority) = a.get("priority").and_then(Value::as_u64) {
                nodes.retain(|v| native_priority(&v["priority"]).unwrap_or(0) == priority);
            }
            nodes.sort_by(priority_cmp);
            let first = a.get("first").and_then(Value::as_u64).unwrap_or(50) as usize;
            let start = if let Some(cursor) = a.get("after").and_then(Value::as_str) {
                let decoded: Value = serde_json::from_str(cursor)
                    .map_err(|_| Fault::new("INVALID_CURSOR", "Priority cursor is invalid"))?;
                require(
                    decoded["version"] == 1 && decoded["group"] == group,
                    "INVALID_CURSOR",
                    "Priority cursor belongs to another scope",
                )?;
                let anchor = decoded["last"]
                    .as_str()
                    .ok_or_else(|| Fault::new("INVALID_CURSOR", "Priority cursor has no anchor"))?;
                nodes
                    .iter()
                    .position(|v| v["id"] == anchor)
                    .map(|i| i + 1)
                    .ok_or_else(|| {
                        Fault::new(
                            "INVALID_CURSOR",
                            "Priority cursor anchor is no longer in this group",
                        )
                    })?
            } else {
                0
            };
            let end = (start + first).min(nodes.len());
            let page_nodes = nodes[start..end].to_vec();
            let has_next = end < nodes.len();
            let end_cursor = page_nodes
                .last()
                .map(|v| json!({"version":1,"group":group,"last":v["id"]}).to_string());
            return Ok(
                json!({"nodes":page_nodes,"pageInfo":{"hasNextPage":has_next,"endCursor":end_cursor},"scope":group}),
            );
        }
        Ok(self.store.linear.call(query, args).await?[field].clone())
    }
    /// Save one native document, using the caller's UUID to recover uncertain creation without duplicates.
    async fn document(&self, a: &Value) -> Result<Value> {
        let editing = a["id"].is_string();
        let id = if editing {
            text(a, "id")?
        } else {
            text(a, "request_id")?
        };
        if editing {
            require(
                a.get("project_id").is_none() && a.get("issue_id").is_none(),
                "INVALID_INPUT",
                "Document ownership cannot be changed by edit",
            )?;
            self.store
                .linear
                .object("QDocument", "document", id)
                .await?;
            let mut input = json!({});
            for k in ["title", "content"] {
                if let Some(v) = a.get(k) {
                    input[k] = v.clone();
                }
            }
            require(
                !input.as_object().unwrap().is_empty(),
                "INVALID_INPUT",
                "No document fields to edit",
            )?;
            return Ok(self
                .store
                .linear
                .call("MUpdateDocument", json!({"id":id,"input":input}))
                .await?["documentUpdate"]["document"]
                .clone());
        }
        require(
            a["project_id"].is_string() != a["issue_id"].is_string(),
            "INVALID_INPUT",
            "A new Document needs exactly one Project or Issue parent",
        )?;
        text(a, "title")?;
        require(
            a.get("content").is_some(),
            "INVALID_INPUT",
            "New Document requires content",
        )?;
        if let Some(p) = a["project_id"].as_str() {
            self.project(p).await?;
        } else {
            self.store.work(text(a, "issue_id")?).await?;
        }
        if let Some(d) = self.store.optional("QDocument", "document", id).await? {
            require(
                d["title"] == a["title"]
                    && markdown_key(d["content"].as_str().unwrap_or(""))
                        == markdown_key(a["content"].as_str().unwrap())
                    && d["project"]["id"] == a["project_id"]
                    && d["issue"]["id"] == a["issue_id"],
                "REQUEST_CONFLICT",
                "Document request_id already names different content",
            )?;
            return Ok(d);
        }
        let mut input = json!({"id":id,"title":a["title"],"content":a["content"]});
        if a["project_id"].is_string() {
            input["projectId"] = a["project_id"].clone();
        } else {
            input["issueId"] = a["issue_id"].clone();
        }
        Ok(self
            .store
            .linear
            .call("MCreateDocument", json!({"input":input}))
            .await?["documentCreate"]["document"]
            .clone())
    }
}
