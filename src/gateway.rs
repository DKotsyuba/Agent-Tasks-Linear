//! Sixteen explicit workflow operations over native Linear entities.
use crate::{
    catalog::Catalog,
    linear::Linear,
    model::{Fault, Kind, Meta, Outcome, Pending, Result, Review, Status, Work, require, text},
    records::{Store, child_id, markdown_key, patch_description, read_fields},
    rules,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
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

/// Compare owned native IDs, relationships, title, labels and Markdown description, ignoring timestamps and normalizing Markdown presentation; compare priority only when the target carries it.
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
        && markdown_key(a["description"].as_str().unwrap_or(""))
            == markdown_key(b["description"].as_str().unwrap_or(""))
}

/// Order native Issue objects by priority 1–4 then 0, ascending `prioritySortOrder`, and ascending UUID; missing or unknown priorities rank with 0 and missing tie order follows known values.
fn priority_cmp(a: &Value, b: &Value) -> std::cmp::Ordering {
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
}
impl Gateway {
    /// Construct a gateway without contacting Linear; tool discovery works before credentials exist.
    pub fn new(linear: Linear) -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            catalog: Catalog::new()?,
            store: Store { linear },
            lock: Mutex::new(()),
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
            "list_items" => self.list(&args, false).await,
            "search" => self.list(&args, true).await,
            "save_document" => self.document(&args).await,
            "move_status" => self.move_status(&args).await,
            "record_review" => self.review(&args).await,
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
    /// Create a permanent native project and ensure its two default documents on every retry.
    async fn create_project(&self, a: &Value) -> Result<Value> {
        let id = text(a, "request_id")?;
        let title = text(a, "title")?;
        let description = text(a, "description")?;
        let repository = text(a, "repository_url")?;
        require(
            repository.starts_with("https://github.com/"),
            "INVALID_INPUT",
            "repository_url must be a GitHub HTTPS repository link",
        )?;
        self.states(text(a, "team_id")?).await?;
        let content = patch_description(
            "",
            &json!({"description":description,"repository_url":repository}),
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
    /// Patch only requested project fields, preserving native content outside owned sections.
    async fn edit_project(&self, a: &Value) -> Result<Value> {
        let id = text(a, "id")?;
        let p = self.project(id).await?;
        let mut input = json!({});
        if let Some(v) = a.get("title") {
            input["name"] = v.clone();
        }
        let mut fields = json!({});
        for key in ["description", "repository_url"] {
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
    /// Create a native issue, then its small attachment. Same-ID retries resume incomplete creation.
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
            && fields.get("repository_url").is_none()
            && let Some(repo) =
                read_fields(p["content"].as_str().unwrap_or(""))?.get("repository_url")
        {
            fields["repository_url"] = repo.clone();
        }
        self.catalog.validate_fields(kind, &fields)?;
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
    /// Concurrent manual changes produce a conflict and are never overwritten on retry.
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
        let native = if already_applied {
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
    /// Apply one managed issue edit for `kind`; missing title normalizes the existing title, missing priority preserves it, and priority zero clears it.
    /// Empty or title/priority-only edits preserve native descriptions and review identity in any status; requirement/parent edits invalidate review and require reopening reviewed work, except the existing Module merge-report allowance. Returns the confirmed issue outcome or a safe conflict/write fault.
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
        self.catalog.validate_fields(kind, &fields)?;
        Self::check_fields(kind, &fields, next.parent_id.as_deref(), &graph)?;
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
    /// Guard and explicitly move work; check_only never persists intent, labels or changes.
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
        let states = self
            .states(w.native["team"]["id"].as_str().unwrap())
            .await?;
        let state = Self::state_id(&states, target)?;
        let mut next = m.clone();
        next.status = target;
        let mut input = json!({"stateId":state});
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
    /// Persist a native comment and current-round review decision; never transition work implicitly.
    async fn review(&self, a: &Value) -> Result<Value> {
        let (w, graph) = self.loaded(text(a, "id")?).await?;
        let m = w.managed()?;
        let request = Self::request("record_review", a);
        if let Some(v) = self.resume(&w, &request).await? {
            return Ok(v);
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
        let body = format!(
            "## Ревью\n\nПроверяющий: {}\n\nВердикт: {}\n\n{}\n\n### Замечания\n{}\n\n### Артефакты\n{}\n",
            text(a, "reviewer")?,
            if a["verdict"] == "accepted" {
                "Принято"
            } else {
                "Нужны изменения"
            },
            text(a, "summary")?,
            a["findings"].as_str().unwrap(),
            a["artifacts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| format!("- {}", v.as_str().unwrap()))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let body = format!("Раунд: {} · Редакция: {}\n\n{}", m.round, m.revision, body);
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
        Ok(json!({"review":next.review,"issue_id":w.id()}))
    }
    /// Read a Project, Issue or Document by validated type and UUID; issue context reuses its loaded Project graph for children, transition conditions and same Project/parent/type-label priority peers.
    /// Reads do not mutate Linear; missing records, unmanaged issues and native/API failures propagate as safe faults.
    async fn context(&self, a: &Value) -> Result<Value> {
        let id = text(a, "id")?;
        match text(a, "type")? {
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
            _ => {
                let (w, g) = self.loaded(id).await?;
                let checkout=rules::parent(&w).and_then(|p|rules::find(&g,p)).filter(|p|p.meta.as_ref().is_some_and(|m|m.kind==Kind::Module)).map(|p|json!({"repository_url":p.fields["repository_url"],"branch":p.fields["branch"],"worktree":p.fields["worktree"],"lead":p.fields["lead"]}));
                let m = w.managed()?;
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
                    json!({"issue":w.native,"fields":w.fields,"workflow":w.meta,"parent_checkout":checkout,"children":rules::children(&g,id).iter().map(|c|&c.native).collect::<Vec<_>>(),"priority_group":priority_group,"discrepancies":rules::discrepancies(&w,&g),"transitions":rules::actions(&w,&g)}),
                )
            }
        }
    }
    /// List or search one entity type; `search` selects native search and `a` supplies the accepted filters/page cursor.
    /// Default lists and searches forward native cursors unchanged. Priority ordering loads the complete filtered Issue group within the Store page budget, validates parent scope, sorts before slicing, and binds JSON cursors to every group filter; malformed, changed-scope or missing-anchor cursors fail with `INVALID_CURSOR`.
    async fn list(&self, a: &Value, search: bool) -> Result<Value> {
        let kind = text(a, "type")?;
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
