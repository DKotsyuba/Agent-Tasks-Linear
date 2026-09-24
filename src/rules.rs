//! Trusted agent activity: record who worked where, what they reported, and links to their results.

use crate::{
    gateway::{Effect, Plan},
    model::{Fault, Outcome, Principal, Record, Result, Role, array, now, require, text},
    records::{Signer, Snapshot},
};
use serde_json::{Value, json};
use uuid::Uuid;

/// Check the authenticated product scope and current assignment; reports never grant authority.
pub fn authorize(s: &Snapshot, p: &Principal) -> Result<()> {
    require(
        (p.role == Role::Owner && p.products.is_empty()) || p.products.contains(&s.product),
        "OUT_OF_SCOPE",
        "Binding does not allow this product",
    )?;
    require(
        s.config.payload["epoch"].as_u64() == Some(p.epoch),
        "UNAUTHORIZED",
        "Binding policy epoch is stale",
    )?;
    let allowed = array(&s.config.payload, "allowed_principals");
    require(
        allowed.is_empty() || allowed.iter().any(|id| id == &p.id),
        "UNAUTHORIZED",
        "Principal was revoked",
    )?;
    if !p.role.controls() && p.role != Role::Observer {
        let a = s.record(p.assignment_id.as_deref().unwrap_or(""), Some("assignment"))?;
        require(
            a.payload["principal_id"] == p.id
                && a.payload["role"] == p.role.name()
                && a.payload["generation"].as_u64() == p.generation
                && a.payload["status"] != "revoked",
            "STALE_ASSIGNMENT",
            "Assignment identity, role or generation changed",
        )?;
    }
    Ok(())
}

/// Limit a worker to its assigned subtree; owner/root and product observers may read product-wide.
pub fn in_scope(s: &Snapshot, p: &Principal, id: &str) -> Result<()> {
    s.work(id)?;
    if p.role.controls() || p.role == Role::Observer {
        return Ok(());
    }
    let a = s.record(p.assignment_id.as_deref().unwrap_or(""), Some("assignment"))?;
    require(
        array(&a.payload, "scope_work_ids")
            .iter()
            .filter_map(Value::as_str)
            .any(|root| s.within(id, root)),
        "OUT_OF_SCOPE",
        "Work is outside this assignment",
    )
}

/// Prevent accidentally writing into a moved/archived work; native status is reported separately.
pub fn no_drift(s: &Snapshot, id: &str) -> Result<()> {
    let w = s.work(id)?;
    require(
        w.native["project"]["id"] == w.identity.payload["native_project_id"]
            && w.native["parent"]["id"] == w.identity.payload["native_parent_id"],
        "STRUCTURE_DRIFT",
        "Work moved to another parent or project",
    )?;
    require(
        w.native["archivedAt"].is_null(),
        "STRUCTURE_DRIFT",
        "Work was archived",
    )
}

/// Resolve the owning work for explicit work, record, publication, or product-level note intents.
pub fn target(s: &Snapshot, args: &Value) -> Result<String> {
    if let Some(id) = args["work_id"]
        .as_str()
        .or(args["parent_id"].as_str())
        .or(args["epic_id"].as_str())
    {
        return Ok(id.into());
    }
    for field in ["note_id", "record_id"] {
        if let Some(id) = args[field].as_str() {
            return Ok(s.record(id, None)?.work_id.clone());
        }
    }
    Ok(s.product.clone())
}

/// Latest reported activity, including attribution; old records remain readable without new certificates.
pub fn activity(s: &Snapshot, work: &str) -> Value {
    let Some(w) = s.works.get(work) else {
        return json!({});
    };
    if w.head.payload["activity"].is_object() {
        return w.head.payload["activity"].clone();
    }
    let mut out = json!({"execution":{},"artifacts":[]});
    for field in [
        "result_id",
        "acceptance_id",
        "current_submission_id",
        "checkpoint_id",
    ] {
        if let Some(r) = w.head.payload[field]
            .as_str()
            .and_then(|id| s.records.get(id))
        {
            out["summary"] = r.payload["summary"]
                .as_str()
                .or(r.payload["reason"].as_str())
                .map(Value::from)
                .unwrap_or(Value::Null);
            out["artifacts"] = r.payload["artifacts"].clone();
            out["recorded_by"] = r.actor["principal_id"].clone();
            out["recorded_at"] = json!(r.created_at);
            break;
        }
    }
    out
}

/// A finite mutation recipe; receipts handle network uncertainty outside these trusted-report rules.
struct Builder<'a> {
    /// Current external facts for this request only.
    s: &'a Snapshot,
    /// Authenticated actor, separate from any reported external agent name.
    p: &'a Principal,
    /// Internal record-envelope authentication; never an artifact correctness proof.
    signer: &'a Signer,
    /// Existing work whose history and native status this intent updates.
    work: String,
    /// Mutable copy of that work's current head.
    head: Record,
    /// Reserved external actions and the response to retain for replay.
    plan: Plan,
}
impl<'a> Builder<'a> {
    /// Start from current facts; an optional revision prevents accidental stale writes without requiring hashes.
    fn new(
        s: &'a Snapshot,
        p: &'a Principal,
        signer: &'a Signer,
        args: &Value,
        work: &str,
    ) -> Result<Self> {
        in_scope(s, p, work)?;
        no_drift(s, work)?;
        let head = s.work(work)?.head.clone();
        if let Some(revision) = args["expected"]["work_revision"].as_u64() {
            require(
                revision == head.revision,
                "STALE_CONTEXT",
                "Work changed; refresh context",
            )?;
        }
        Ok(Self {
            s,
            p,
            signer,
            work: work.into(),
            head,
            plan: Plan {
                target_work_id: work.into(),
                effects: vec![],
                documents: vec![],
                result: Outcome::ok(json!({})),
                actor: p.clone(),
            },
        })
    }
    /// Add one record upsert to its real owning issue; no external writes happen while planning.
    fn put(&mut self, record: Record) -> Result<()> {
        let base_url = text(
            &self.s.work(&record.work_id)?.identity.payload,
            "record_base_url",
        )?
        .into();
        self.plan.effects.push(Effect::Record { record, base_url });
        Ok(())
    }
    /// Append an attributed immutable history event and return its reserved identity.
    fn add(&mut self, kind: &str, payload: Value) -> Result<Record> {
        let record =
            self.signer
                .record(self.p, &self.s.product, &self.work, kind, payload, None)?;
        self.put(record.clone())?;
        Ok(record)
    }
    /// Update a mutable pointer record while keeping history events in their original records.
    fn update(&mut self, mut record: Record, payload: Value) -> Result<Record> {
        record.payload = payload;
        record.revision += 1;
        record.created_at = now();
        record.actor =
            json!({"principal_id":self.p.id,"role":self.p.role,"generation":self.p.generation});
        self.signer.seal(&mut record)?;
        self.put(record.clone())?;
        Ok(record)
    }
    /// Preserve optional planning/review material in a native Document, without content certification.
    fn document(&mut self, kind: &str, title: &str, mut payload: Value) -> Result<Record> {
        let id = Uuid::new_v4().to_string();
        let content = format!(
            "# {title}\n\n{}",
            serde_json::to_string_pretty(&payload).unwrap()
        );
        self.plan.effects.push(Effect::CreateDocument {input:json!({
            "id":id,"title":title,"content":content,"projectId":self.s.config.payload["general_project_id"]
        })});
        payload["document_id"] = json!(id);
        self.add(kind, payload)
    }
    /// Add a human-readable activity comment so the trace is visible in Linear without decoding metadata.
    fn comment(&mut self, heading: &str, summary: &str, trace: &Value) {
        let mut body = format!("## {heading}\n\n{summary}\n\nRecorded by: {}\n", self.p.id);
        if let Some(execution) = trace["execution"].as_object() {
            for (key, value) in execution {
                if let Some(value) = value.as_str() {
                    body.push_str(&format!("\n- {key}: {value}"));
                }
            }
        }
        for artifact in array(trace, "artifacts") {
            body.push_str(&format!(
                "\n- {}: {}",
                artifact["kind"].as_str().unwrap_or("artifact"),
                artifact["locator"].as_str().unwrap_or("")
            ));
            if let Some(commit) = artifact["commit"].as_str() {
                body.push_str(&format!(" (commit {commit})"));
            }
        }
        self.plan.effects.push(Effect::CreateComment {
            input: json!({
                "id":Uuid::new_v4().to_string(),"issueId":self.work,"body":body
            }),
        });
    }
    /// Set the reported status on the issue and its epic Project; this does not claim independent acceptance.
    fn state(&mut self, state: &str) -> Result<()> {
        let id = self.s.config.payload["issue_state_ids"][state].clone();
        require(
            id.is_string(),
            "CONFIG_INVALID",
            "Workflow status mapping is missing",
        )?;
        self.head.payload["state"] = json!(state);
        self.head.payload["native_state_id"] = id.clone();
        self.plan.effects.push(Effect::UpdateIssue {
            id: self.work.clone(),
            input: json!({"stateId":id}),
        });
        let work = self.s.work(&self.work)?;
        if let Some(project) = work.identity.payload["epic_project_id"].as_str() {
            let id = self.s.config.payload["project_status_ids"][state].clone();
            if id.is_string() {
                self.plan.effects.push(Effect::UpdateProject {
                    id: project.into(),
                    input: json!({"statusId":id}),
                });
            }
        }
        Ok(())
    }
    /// Find an explicit/current assignment, permitting an inherited module lead for its tasks.
    fn assignment(&self, args: &Value) -> Result<Record> {
        let id = args["assignment_id"]
            .as_str()
            .or(self.p.assignment_id.as_deref());
        let record = if let Some(id) = id {
            self.s.record(id, Some("assignment"))?
        } else {
            self.s
                .records
                .values()
                .filter(|r| {
                    r.record_kind == "assignment"
                        && r.payload["status"] != "revoked"
                        && self.s.within(&self.work, &r.work_id)
                })
                .max_by_key(|r| (r.work_id == self.work, r.created_at.clone()))
                .ok_or_else(|| Fault::new("ASSIGNMENT_REQUIRED", "Assign an agent first"))?
        };
        require(
            self.s.within(&self.work, &record.work_id) && record.payload["status"] != "revoked",
            "OUT_OF_SCOPE",
            "Assignment does not cover this work",
        )?;
        if !self.p.role.controls() {
            require(
                record.payload["principal_id"] == self.p.id,
                "UNAUTHORIZED",
                "Use your own assignment",
            )?;
        }
        Ok(record.clone())
    }
    /// Merge this run's reported context; a fresh begin keeps old runs only in history, never as current facts.
    fn trace(&mut self, args: &Value, summary: &str, completed: bool, fresh: bool) -> Value {
        let mut trace = if fresh {
            json!({})
        } else {
            activity(self.s, &self.work)
        };
        let mut execution = trace["execution"].as_object().cloned().unwrap_or_default();
        if let Ok(assignment) = self.assignment(args) {
            if let Some(values) = assignment.payload["execution"].as_object() {
                execution.extend(values.clone());
            }
            if fresh {
                execution.remove("run_id");
                execution.remove("run_url");
            }
            execution.insert("agent".into(), assignment.payload["principal_id"].clone());
            if let Some(worktree) = assignment.payload["workspace_ref"].as_str() {
                execution
                    .entry("worktree")
                    .or_insert_with(|| json!(worktree));
            }
        }
        if let Some(worktree) = args["workspace_ref"].as_str() {
            execution.insert("worktree".into(), json!(worktree));
        }
        if let Some(values) = args["execution"].as_object() {
            execution.extend(values.clone());
        }
        execution.entry("agent").or_insert_with(|| json!(self.p.id));
        let mut artifacts = array(&trace, "artifacts").to_vec();
        for item in array(args, "artifacts") {
            if !artifacts.contains(item) {
                artifacts.push(item.clone());
            }
        }
        trace["execution"] = json!(execution);
        trace["artifacts"] = json!(artifacts);
        trace["summary"] = json!(summary);
        trace["recorded_by"] = json!(self.p.id);
        trace["recorded_at"] = json!(now());
        trace["basis"] = json!("agent_report");
        if completed {
            trace["completed_by"] = json!(self.p.id);
        }
        self.head.payload["activity"] = trace.clone();
        trace
    }
    /// Record completion directly from the agent report; optional review never blocks this action.
    fn complete(&mut self, args: &Value, summary: &str) -> Result<()> {
        let kind = self.s.work(&self.work)?.identity.payload["kind"].clone();
        require(
            kind != "product",
            "INVALID_PARENT",
            "Complete a work item, not the product container",
        )?;
        let trace = self.trace(args, summary, true, false);
        let result = self.add("result", json!({"summary":summary,"artifacts":trace["artifacts"],
            "execution":trace["execution"],"basis":"agent_report","submission_id":self.head.payload["current_submission_id"]}))?;
        self.head.payload["result_id"] = json!(result.record_id);
        self.head.payload["acceptance_id"] = json!(result.record_id);
        self.state("accepted")?;
        self.comment("Completed — agent report", summary, &trace);
        self.plan.result.data = json!({"result_id":result.record_id,"acceptance_id":result.record_id,
            "acceptance_level":if kind=="task" {json!("task_local")}else{kind},"state":"accepted","activity":trace});
        Ok(())
    }
    /// Persist the latest activity head and return direct navigation and simple next actions.
    fn finish(mut self, key: &str) -> Result<Plan> {
        self.head.revision += 1;
        self.head.created_at = now();
        self.head.payload["last_committed_operation"] = json!(key);
        self.signer.seal(&mut self.head)?;
        self.plan.result.status = "committed".into();
        self.plan.result.operation_key = Some(key.into());
        self.plan.result.version = json!({"work_revision":self.head.revision});
        if !self.plan.result.data["created_work_id"].is_string() {
            self.plan.result.data["work_id"] = json!(self.work);
            self.plan.result.data["url"] = self.s.work(&self.work)?.native["url"].clone();
        } else {
            self.plan.result.data["parent_revision"] = json!(self.head.revision);
            self.plan.result.version = json!({"work_revision":1});
        }
        self.plan.result.available_actions.push(json!({"tool":"at_context",
            "work_id":self.plan.result.data["work_id"],"reason":"Read the work and activity history"}));
        self.put(self.head.clone())?;
        Ok(self.plan)
    }
}

/// Remove transport-only fields before retaining optional material as a reported history event.
fn reported(args: &Value) -> Value {
    let mut value = args.clone();
    for key in ["product_id", "idempotency_key", "expected"] {
        value.as_object_mut().unwrap().remove(key);
    }
    value["basis"] = json!("agent_report");
    value
}

/// Build the closed set of activity intents; no proof hashes, mandatory plans, or review gates are required.
pub fn plan(
    s: &Snapshot,
    p: &Principal,
    signer: &Signer,
    name: &str,
    args: &Value,
) -> Result<Plan> {
    let work = target(s, args)?;
    let key = text(args, "idempotency_key")?;
    let mut b = Builder::new(s, p, signer, args, &work)?;
    match name {
        "at_work_create" => {
            let kind = text(args, "kind")?;
            let parent = s.work(&work)?;
            let pk = text(&parent.identity.payload, "kind")?;
            require(
                match kind {
                    "epic" => pk == "product",
                    "module" => matches!(pk, "product" | "epic"),
                    "task" => pk == "module",
                    "atomic" => pk != "atomic",
                    _ => false,
                },
                "INVALID_PARENT",
                "Unsupported work parent",
            )?;
            if pk == "task" {
                require(
                    s.config.payload["policy"]["nested_atomic_verified"] == true,
                    "SCHEMA_UNSUPPORTED",
                    "Nested atomic hierarchy is not enabled for this product",
                )?;
            }
            let id = Uuid::new_v4().to_string();
            let mut project = if pk == "epic" {
                parent.identity.payload["epic_project_id"].clone()
            } else {
                parent.identity.payload["native_project_id"].clone()
            };
            let epic_project = if kind == "epic" {
                let pid = Uuid::new_v4().to_string();
                b.plan.effects.push(Effect::CreateProject {input:json!({"id":pid,"name":args["title"],
                    "content":args["description"].as_str().unwrap_or(""),"teamIds":s.config.payload["team_ids"]})});
                b.plan.effects.push(Effect::LinkProject {
                    input: json!({"id":Uuid::new_v4().to_string(),
                    "initiativeId":s.config.payload["initiative_id"],"projectId":pid}),
                });
                project = s.config.payload["general_project_id"].clone();
                Some(pid)
            } else {
                None
            };
            let native_parent =
                if kind == "task" || (kind == "atomic" && matches!(pk, "module" | "task")) {
                    json!(work)
                } else {
                    Value::Null
                };
            let state = s.config.payload["issue_state_ids"]["draft"].clone();
            let label = if kind == "epic" {
                "epic_companion"
            } else {
                kind
            };
            let labels = [
                s.config.payload["kind_label_ids"][label].clone(),
                s.config.payload["managed_label_id"].clone(),
            ]
            .into_iter()
            .filter(Value::is_string)
            .collect::<Vec<_>>();
            b.plan.effects.push(Effect::CreateIssue {input:json!({"id":id,"title":args["title"],
                "description":args["description"].as_str().unwrap_or(""),"teamId":s.config.payload["team_ids"][0],
                "projectId":project,"parentId":native_parent,"stateId":state,"labelIds":labels})});
            let identity = signer.record(p,&s.product,&id,"identity",json!({"kind":kind,"primary_parent_id":work,
                "native_issue_id":id,"native_project_id":project,"native_parent_id":native_parent,
                "epic_project_id":epic_project,"record_base_url":"$created_issue_url",
                "classification":args["classification"].as_str().unwrap_or("feature"),
                "mandatory":args["mandatory"].as_bool().unwrap_or(true),"replacement_for":args["replacement_for"]}),None)?;
            let head = signer.record(
                p,
                &s.product,
                &id,
                "work_head",
                json!({"state":"draft",
                "native_state_id":state,"children":[],"assignment_ids":[]}),
                None,
            )?;
            b.plan.effects.push(Effect::NewWork {
                identity,
                head: Box::new(head),
            });
            let mut children = array(&b.head.payload, "children").to_vec();
            children.push(json!(id));
            b.head.payload["children"] = json!(children);
            b.plan.result.data = json!({"work_id":id,"created_work_id":id,"native_issue_id":id,
                "native_project_id":epic_project.map(Value::String).unwrap_or(project),"state":"draft"});
        }
        "at_plan_publish" => {
            let mut payload = reported(args);
            payload["plan_version"] =
                json!(b.head.payload["plan_version"].as_u64().unwrap_or(0) + 1);
            let r = b.document("plan", "Work plan", payload)?;
            b.head.payload["plan_record_id"] = json!(r.record_id);
            b.head.payload["plan_version"] = r.payload["plan_version"].clone();
            if b.head.payload["state"] == "draft" {
                b.state("ready")?;
            }
            b.plan.result.data =
                json!({"plan_id":r.record_id,"plan_version":r.payload["plan_version"]});
        }
        "at_assign" => {
            let who = text(args, "principal_id")?;
            let role = args["role"].as_str().unwrap_or("lead");
            for id in array(&b.head.payload, "assignment_ids") {
                let old = s.record(id.as_str().unwrap_or(""), Some("assignment"))?;
                if old.payload["status"] != "revoked" && old.payload["role"] == role {
                    require(
                        old.payload["principal_id"] == who,
                        "ASSIGNMENT_EXISTS",
                        "Use transfer to change the assigned agent",
                    )?;
                    b.plan.result.data = json!({"assignment_id":old.record_id,"generation":old.payload["generation"],"principal_id":who});
                    b.plan.result.status = "noop".into();
                    b.plan.result.operation_key = Some(key.into());
                    b.plan.result.data["work_id"] = json!(work);
                    return Ok(b.plan);
                }
            }
            let r = b.add("assignment",json!({"principal_id":who,"role":role,"scope_work_ids":[work],
                "scope_text":args["scope"].as_str().unwrap_or(""),"generation":1,"status":"active",
                "attempt_ids":[],"execution":args["execution"],"workspace_ref":args["workspace_ref"]}))?;
            let mut ids = array(&b.head.payload, "assignment_ids").to_vec();
            ids.push(json!(r.record_id));
            b.head.payload["assignment_ids"] = json!(ids);
            let mut update = json!({});
            for (from, to) in [
                ("human_assignee_id", "assigneeId"),
                ("delegate_app_user_id", "delegateId"),
            ] {
                if args[from].is_string() {
                    update[to] = args[from].clone();
                }
            }
            if !update.as_object().unwrap().is_empty() {
                b.plan.effects.push(Effect::UpdateIssue {
                    id: work.clone(),
                    input: update,
                });
            }
            let mut trace_args = args.clone();
            if !trace_args["execution"].is_object() {
                trace_args["execution"] = json!({});
            }
            if trace_args["execution"]["agent"].is_null() {
                trace_args["execution"]["agent"] = json!(who);
            }
            let trace = b.trace(&trace_args, &format!("Assigned to {who}"), false, false);
            b.comment("Assigned", &format!("Agent: {who}"), &trace);
            b.plan.result.data =
                json!({"assignment_id":r.record_id,"generation":1,"principal_id":who,"role":role});
        }
        "at_execution_observe" => {
            let a = b.assignment(args)?;
            let mut observation = args["observation"].clone();
            if observation["observed_at"].is_null() {
                observation["observed_at"] = json!(now());
            }
            let r = b.add(
                "attempt",
                json!({"assignment_id":a.record_id,
                "generation":a.payload["generation"],"observation":observation}),
            )?;
            let mut payload = a.payload.clone();
            let mut ids = array(&payload, "attempt_ids").to_vec();
            ids.push(json!(r.record_id));
            payload["attempt_ids"] = json!(ids);
            payload["latest_attempt_id"] = json!(r.record_id);
            b.update(a, payload)?;
            let mut context = args.clone();
            if !context["execution"].is_object() {
                context["execution"] = json!({});
            }
            for field in ["runtime", "run_id"] {
                if context["execution"][field].is_null() {
                    context["execution"][field] = observation[field].clone();
                }
            }
            b.trace(&context, "Runtime observation recorded", false, false);
            b.plan.result.data =
                json!({"attempt_id":r.record_id,"runtime_state":observation["state"]});
        }
        "at_begin" => {
            let a = b.assignment(args)?;
            let mut trace = b.trace(args, "Work started", false, true);
            trace.as_object_mut().unwrap().remove("completed_by");
            b.head.payload["activity"] = trace.clone();
            let r = b.add(
                "begin",
                json!({"assignment_id":a.record_id,"execution":trace["execution"],
                "attempt_id":args["attempt_id"]}),
            )?;
            b.head.payload["begin_id"] = json!(r.record_id);
            b.head.payload["activity"]
                .as_object_mut()
                .unwrap()
                .remove("completed_by");
            b.state("in_progress")?;
            b.comment("Started", "Work started", &trace);
            b.plan.result.data =
                json!({"begin_id":r.record_id,"activity":trace,"state":"in_progress"});
        }
        "at_checkpoint" => {
            let summary = text(args, "summary")?;
            let trace = b.trace(args, summary, false, false);
            let mut payload = reported(args);
            payload["execution"] = trace["execution"].clone();
            let r = b.add("checkpoint", payload)?;
            b.head.payload["checkpoint_id"] = json!(r.record_id);
            b.comment("Progress", summary, &trace);
            b.plan.result.data = json!({"checkpoint_id":r.record_id,"activity":trace});
        }
        "at_complete" | "at_task_complete" => {
            let summary = text(
                args,
                if name == "at_task_complete" {
                    "result_summary"
                } else {
                    "summary"
                },
            )?;
            b.complete(args, summary)?;
        }
        "at_submit" => {
            let summary = text(args, "summary")?;
            let trace = b.trace(args, summary, false, false);
            let r = b.add(
                "submission",
                json!({"summary":summary,"artifacts":trace["artifacts"],
                "execution":trace["execution"],"evidence":args["evidence"],"basis":"agent_report"}),
            )?;
            b.head.payload["current_submission_id"] = json!(r.record_id);
            b.state("review")?;
            b.comment("Submitted for optional review", summary, &trace);
            b.plan.result.data =
                json!({"submission_id":r.record_id,"state":"review","activity":trace});
        }
        "at_review_open" => {
            let reviewer = text(args, "reviewer_principal")?;
            let submission = args["submission_id"]
                .as_str()
                .or(b.head.payload["current_submission_id"].as_str())
                .map(str::to_owned);
            if let Some(id) = &submission {
                require(
                    s.record(id, Some("submission"))?.work_id == work,
                    "OUT_OF_SCOPE",
                    "Submission belongs to another work",
                )?;
            }
            let mut current = None;
            if let Some(id) = b.head.payload["review_case_id"].as_str() {
                let case = s.record(id, Some("review_case"))?;
                let old = s
                    .record(
                        text(&case.payload, "reviewer_assignment_id")?,
                        Some("assignment"),
                    )?
                    .clone();
                if old.payload["principal_id"] == reviewer && old.payload["status"] != "revoked" {
                    current = Some(old);
                } else {
                    let mut payload = old.payload.clone();
                    payload["status"] = json!("revoked");
                    b.update(old, payload)?;
                }
            }
            let a = if let Some(current) = current {
                current
            } else {
                b.add("assignment",json!({"principal_id":reviewer,"role":if args["review_kind"]=="composition"{"integrator"}else{"reviewer"},
                    "scope_work_ids":[work],"generation":1,"status":"active","attempt_ids":[]}))?
            };
            let mut ids = array(&b.head.payload, "assignment_ids").to_vec();
            if !ids.contains(&json!(a.record_id)) {
                ids.push(json!(a.record_id));
            }
            b.head.payload["assignment_ids"] = json!(ids);
            let r = if let Some(id) = b.head.payload["review_case_id"].as_str() {
                let old = s.record(id, Some("review_case"))?.clone();
                let mut payload = old.payload.clone();
                payload["reviewer_assignment_id"] = json!(a.record_id);
                payload["current_submission_id"] = json!(submission);
                payload["status"] = json!("open");
                b.update(old, payload)?
            } else {
                b.add(
                    "review_case",
                    json!({"reviewer_assignment_id":a.record_id,"current_submission_id":submission,
                    "round_ids":[],"finding_ids":[],"status":"open","scope":args["scope"]}),
                )?
            };
            b.head.payload["review_case_id"] = json!(r.record_id);
            b.comment(
                "Optional review",
                &format!("Reviewer: {reviewer}"),
                &json!({}),
            );
            b.plan.result.data = json!({"case_id":r.record_id,"reviewer_assignment_id":a.record_id,"generation":a.payload["generation"]});
        }
        "at_review_report" => {
            let case = s
                .record(text(args, "case_id")?, Some("review_case"))?
                .clone();
            require(
                case.work_id == work,
                "OUT_OF_SCOPE",
                "Review case belongs to another work",
            )?;
            require(
                p.assignment_id.as_deref() == case.payload["reviewer_assignment_id"].as_str(),
                "STALE_ASSIGNMENT",
                "This review belongs to another reviewer",
            )?;
            let r = b.add("review_round", reported(args))?;
            let mut payload = case.payload.clone();
            let mut rounds = array(&payload, "round_ids").to_vec();
            rounds.push(json!(r.record_id));
            payload["round_ids"] = json!(rounds);
            payload["latest_summary"] = args["summary"].clone();
            b.update(case, payload)?;
            b.comment("Review report", text(args, "summary")?, &json!({}));
            b.plan.result.data = json!({"review_round_id":r.record_id,"case_id":args["case_id"]});
        }
        "at_accept" => {
            let mut completion = args.clone();
            if let Some(id) = args["submission_id"]
                .as_str()
                .or(b.head.payload["current_submission_id"].as_str())
            {
                let r = s.record(id, Some("submission"))?;
                require(
                    r.work_id == work,
                    "OUT_OF_SCOPE",
                    "Submission belongs to another work",
                )?;
                completion["artifacts"] = r.payload["artifacts"].clone();
                completion["execution"] = r.payload["execution"].clone();
            }
            b.complete(&completion, text(args, "reason")?)?;
        }
        "at_knowledge_save" => {
            let mut payload = reported(args);
            payload["associations"] = json!(array(args, "associations"));
            for association in array(args, "associations") {
                in_scope(s, p, text(association, "work_id")?)?;
            }
            let r = if let Some(id) = args["note_id"].as_str() {
                let old = s.record(id, Some("note"))?.clone();
                b.plan.effects.push(Effect::UpdateDocument {
                    id: text(&old.payload, "document_id")?.into(),
                    input: json!({"title":args["title"],"content":args["content"]}),
                    previous_hash: String::new(),
                });
                payload["document_id"] = old.payload["document_id"].clone();
                payload["publication_ids"] = old.payload["publication_ids"].clone();
                payload["latest_publication_id"] = old.payload["latest_publication_id"].clone();
                payload.as_object_mut().unwrap().remove("content");
                b.update(old, payload)?
            } else {
                let id = Uuid::new_v4().to_string();
                b.plan.effects.push(Effect::CreateDocument {
                    input: json!({"id":id,"title":args["title"],
                    "content":args["content"],"projectId":s.config.payload["general_project_id"]}),
                });
                payload["document_id"] = json!(id);
                payload["publication_ids"] = json!([]);
                payload.as_object_mut().unwrap().remove("content");
                b.add("note", payload)?
            };
            b.plan.result.data =
                json!({"note_id":r.record_id,"document_id":r.payload["document_id"]});
        }
        "at_knowledge_publish" => {
            let note = s.record(text(args, "note_id")?, Some("note"))?.clone();
            let document_id = Uuid::new_v4().to_string();
            b.plan.effects.push(Effect::CopyDocument {
                source_id: text(&note.payload, "document_id")?.into(),
                id: document_id.clone(),
                title: format!("{} · publication", text(&note.payload, "title")?),
                project_id: text(&s.config.payload, "general_project_id")?.into(),
            });
            let r = b.add("publication",json!({"note_id":note.record_id,"document_id":document_id,
                "version":array(&note.payload,"publication_ids").len()+1,"associations":note.payload["associations"],
                "reason":args["reason"],"basis":"agent_report"}))?;
            let mut payload = note.payload.clone();
            let mut ids = array(&payload, "publication_ids").to_vec();
            ids.push(json!(r.record_id));
            payload["publication_ids"] = json!(ids);
            payload["latest_publication_id"] = json!(r.record_id);
            b.update(note, payload)?;
            b.plan.result.data = json!({"publication_id":r.record_id,"document_id":document_id});
        }
        "at_question_ask" => {
            for id in array(args, "blocked_work_ids") {
                in_scope(s, p, id.as_str().unwrap_or(""))?;
            }
            let mut payload = reported(args);
            payload["status"] = json!("open");
            payload["decision_token"] = json!(Uuid::new_v4().to_string());
            let r = b.add("question", payload)?;
            b.comment("Question", text(args, "question")?, &json!({}));
            b.plan.result.data =
                json!({"question_id":r.record_id,"decision_token":r.payload["decision_token"]});
        }
        "at_owner_decide" => {
            let r = s.record(text(args, "record_id")?, None)?.clone();
            require(
                matches!(
                    r.record_kind.as_str(),
                    "question" | "change_proposal" | "finding"
                ),
                "INVALID_INPUT",
                "This record cannot receive a decision",
            )?;
            let source = args["source_kind"]
                .as_str()
                .unwrap_or(if p.role == Role::Owner {
                    "owner_authenticated"
                } else {
                    "root_attributed"
                });
            require(
                source != "owner_authenticated" || p.role == Role::Owner,
                "UNAUTHORIZED",
                "Owner decisions require the owner binding",
            )?;
            if source == "linear_comment" {
                require(
                    s.config.payload["policy"]["integration_uses_owner_pat"] == false,
                    "UNTRUSTED_OWNER_SOURCE",
                    "A PAT-created comment cannot prove human approval",
                )?;
                b.plan.effects.push(Effect::VerifyOwnerComment {
                    id: text(args, "source_comment_id")?.into(),
                    work_id: r.work_id.clone(),
                    decision_token: text(&r.payload, "decision_token")?.into(),
                    decision_key: text(args, "decision_key")?.into(),
                    owner_ids: array(&s.config.payload, "owner_linear_user_ids")
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect(),
                });
            }
            let decision = b.add("decision",json!({"target_record_id":r.record_id,"decision_key":args["decision_key"],
                "rationale":args["rationale"],"source_kind":source,"source_reference":args["source_reference"]}))?;
            let mut payload = r.payload.clone();
            payload["status"] = json!("answered");
            payload["decision_id"] = json!(decision.record_id);
            b.update(r, payload)?;
            b.comment("Decision", text(args, "rationale")?, &json!({}));
            b.plan.result.data = json!({"decision_id":decision.record_id});
        }
        "at_transfer" => {
            let old = b.assignment(args)?;
            require(
                old.work_id == work,
                "OUT_OF_SCOPE",
                "Transfer the work that owns this assignment",
            )?;
            let mut payload = old.payload.clone();
            let generation = payload["generation"].as_u64().unwrap_or(1) + 1;
            let previous = payload["principal_id"].clone();
            payload["principal_id"] = args["new_principal_id"].clone();
            payload["generation"] = json!(generation);
            payload["status"] = json!("active");
            payload["previous_principal_id"] = previous.clone();
            payload["execution"] = json!({"agent":args["new_principal_id"]});
            payload["workspace_ref"] = Value::Null;
            b.update(old.clone(), payload)?;
            b.head.payload["activity"] = json!({"execution":{"agent":args["new_principal_id"]},
                "artifacts":array(args,"preserved_artifacts"),"summary":args["reason"],
                "recorded_by":p.id,"recorded_at":now(),"basis":"agent_report"});
            let r = b.add(
                "transfer",
                json!({"assignment_id":old.record_id,"previous_principal_id":previous,
                "new_principal_id":args["new_principal_id"],"new_generation":generation,
                "reason":args["reason"],"preserved_artifacts":args["preserved_artifacts"]}),
            )?;
            b.comment(
                "Responsibility transferred",
                text(args, "reason")?,
                &json!({}),
            );
            b.plan.result.data = json!({"transfer_id":r.record_id,"assignment_id":old.record_id,
                "new_generation":generation,"status":"active","runtime_stopped":false});
        }
        "at_work_retire" => {
            if let Some(id) = args["replacement_work_id"].as_str() {
                in_scope(s, p, id)?;
            }
            let r = b.add("retirement", reported(args))?;
            b.state(text(args, "disposition")?)?;
            b.comment("Work retired", text(args, "reason")?, &json!({}));
            b.plan.result.data = json!({"retirement_id":r.record_id,"state":args["disposition"]});
        }
        "at_candidate_register" => {
            require(
                s.work(&work)?.identity.payload["kind"] == "epic",
                "INVALID_PARENT",
                "A composition belongs to an epic",
            )?;
            let mut included = vec![];
            for id in array(args, "included_submissions") {
                let r = s.record(id.as_str().unwrap_or(""), Some("submission"))?;
                require(
                    s.within(&r.work_id, &work),
                    "OUT_OF_SCOPE",
                    "Included result belongs to another epic",
                )?;
                included.push(json!({"work_id":r.work_id,"submission_id":r.record_id}));
            }
            let mut payload = reported(args);
            payload["included_results"] = json!(included);
            let r = b.add("candidate", payload)?;
            b.head.payload["candidate_id"] = json!(r.record_id);
            b.plan.result.data = json!({"candidate_id":r.record_id,"included_results":included,"basis":"agent_report"});
        }
        "at_integration_record" => {
            if let Some(id) = args["acceptance_id"].as_str() {
                require(
                    s.record(id, None)?.work_id == work,
                    "OUT_OF_SCOPE",
                    "Result belongs to another work",
                )?;
            }
            let r = b.add("integration", reported(args))?;
            b.comment(
                "Integration reported",
                &format!("Target: {}", text(&args["target"], "locator")?),
                &json!({"artifacts":[args["target"]]}),
            );
            b.plan.result.data = json!({"integration_id":r.record_id,"target":args["target"],"basis":"agent_report"});
        }
        "at_contract_confirm"
        | "at_change_propose"
        | "at_recovery_report"
        | "at_recovery_confirm" => {
            let (kind, id_key) = match name {
                "at_contract_confirm" => ("contract_attestation", "attestation_id"),
                "at_change_propose" => ("change_proposal", "proposal_id"),
                "at_recovery_report" => ("recovery_report", "recovery_report_id"),
                _ => ("recovery_confirmation", "confirmation_id"),
            };
            let r = b.add(kind, reported(args))?;
            b.plan.result.data = json!({id_key:r.record_id,"basis":"agent_report"});
        }
        _ => return Err(Fault::new("UNKNOWN_TOOL", "Unknown activity intent")),
    }
    b.finish(key)
}
