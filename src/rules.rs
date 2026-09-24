//! Workflow guards and bounded mutation recipes, independent of transport sessions.

use crate::{
    gateway::{Effect, Plan},
    model::{Fault, Outcome, Principal, Record, Result, Role, array, require, text},
    records::{Signer, Snapshot, content_hash, hash},
};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use uuid::Uuid;

/// Validate product authorization and current assignment before reads and receipt replay.
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
        allowed.is_empty() || allowed.iter().any(|x| x == &p.id),
        "UNAUTHORIZED",
        "Principal was revoked",
    )?;
    if !p.role.controls() && p.role != Role::Observer {
        let a = s.record(
            p.assignment_id.as_deref().ok_or_else(|| {
                Fault::new(
                    "STALE_ASSIGNMENT",
                    "Provision this binding with its assignment ID",
                )
            })?,
            Some("assignment"),
        )?;
        let committed = s
            .latest_committed(&a.record_id)
            .ok_or_else(|| Fault::new("STALE_ASSIGNMENT", "Assignment has not committed"))?;
        require(
            committed["payload"]["principal_id"] == p.id
                && committed["payload"]["role"] == p.role.name()
                && committed["payload"]["generation"].as_u64() == p.generation
                && committed["payload"]["status"] != "revoked"
                && committed["payload"]["scope_work_ids"] == a.payload["scope_work_ids"],
            "STALE_ASSIGNMENT",
            "Binding differs from the latest committed assignment",
        )?;
        require(
            a.payload["principal_id"] == p.id
                && a.payload["role"] == p.role.name()
                && a.payload["generation"].as_u64() == p.generation
                && a.payload["status"] != "revoked",
            "STALE_ASSIGNMENT",
            "Assignment identity, role or generation is no longer current",
        )?;
    }
    Ok(())
}

/// Check scope by immutable ancestry instead of trusting a client-supplied scope description.
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
        "Work is outside the binding's assignment",
    )
}

/// Compare immutable native relationships and protected status with the signed work head.
pub fn no_drift(s: &Snapshot, id: &str) -> Result<()> {
    let w = s.work(id)?;
    if s.latest_committed(&w.head.record_id).is_some() {
        require(
            s.is_committed(&w.head),
            "STRUCTURE_DRIFT",
            "Work head differs from its latest committed version",
        )?;
    }
    let identity = &w.identity.payload;
    require(
        w.native["project"]["id"] == identity["native_project_id"]
            && w.native["parent"]["id"] == identity["native_parent_id"],
        "STRUCTURE_DRIFT",
        "Native work membership changed outside the gateway",
    )?;
    require(
        w.native["state"]["id"] == w.head.payload["native_state_id"],
        "STRUCTURE_DRIFT",
        "Native status does not match the committed workflow",
    )?;
    require(
        w.native["archivedAt"].is_null(),
        "STRUCTURE_DRIFT",
        "Managed work was archived",
    )
}

/// Internal recipe builder; all proposed effects remain inert until prepare succeeds.
struct Builder<'a> {
    /// Authoritative request-local facts.
    s: &'a Snapshot,
    /// Authenticated caller.
    p: &'a Principal,
    /// Envelope signer; generated effects are integrity protected in the receipt.
    signer: &'a Signer,
    /// Accumulated fixed adapter effects and response.
    plan: Plan,
    /// Work whose revision protects this command.
    work: String,
    /// Mutable head copy finalized after all guards pass.
    head: Record,
}
impl<'a> Builder<'a> {
    /// Start a recipe after resolving the owning work and checking expected revision.
    fn new(
        s: &'a Snapshot,
        p: &'a Principal,
        signer: &'a Signer,
        args: &Value,
        work: &str,
        allow_terminal: bool,
    ) -> Result<Self> {
        in_scope(s, p, work)?;
        no_drift(s, work)?;
        let head = s.work(work)?.head.clone();
        require(
            args["expected"]["work_revision"].as_u64() == Some(head.revision),
            "STALE_CONTEXT",
            "Refresh context and provide the current work revision",
        )?;
        if let Some(v) = args["expected"]["plan_hash"].as_str() {
            require(
                head.payload["plan_hash"] == v,
                "STALE_PLAN",
                "Expected plan hash changed",
            )?;
        }
        if let Some(v) = args["expected"]["assignment_generation"].as_u64()
            && let Some(a) = p.assignment_id.as_deref()
        {
            require(
                s.record(a, Some("assignment"))?.payload["generation"].as_u64() == Some(v),
                "STALE_ASSIGNMENT",
                "Expected generation changed",
            )?;
        }
        require(
            allow_terminal
                || !matches!(
                    head.payload["state"].as_str(),
                    Some("accepted" | "skipped" | "cancelled")
                ),
            "IMMUTABLE_ACCEPTANCE",
            "Terminal work requires a new corrective work item",
        )?;
        Ok(Self {
            s,
            p,
            signer,
            plan: Plan {
                target_work_id: work.into(),
                effects: vec![],
                documents: vec![],
                result: Outcome::ok(json!({})),
                actor: p.clone(),
            },
            work: work.into(),
            head,
        })
    }
    /// Return the stable base URL preserved at work creation.
    fn base(&self, work: &str) -> Result<String> {
        Ok(text(&self.s.work(work)?.identity.payload, "record_base_url")?.into())
    }
    /// Add an immutable record and return its preallocated identity.
    fn add(&mut self, kind: &str, payload: Value, id: Option<String>) -> Result<Record> {
        let r = self
            .signer
            .record(self.p, &self.s.product, &self.work, kind, payload, id)?;
        self.put(r.clone())?;
        Ok(r)
    }
    /// Add an exact record upsert at the owning work's stable URL.
    fn put(&mut self, r: Record) -> Result<()> {
        self.plan.effects.push(Effect::Record {
            base_url: self.base(&r.work_id)?,
            record: r,
        });
        Ok(())
    }
    /// Update a mutable record once, preserving its original identity and incrementing revision.
    fn update(&mut self, mut r: Record, payload: Value) -> Result<Record> {
        r.payload = payload;
        r.revision += 1;
        r.created_at = crate::model::now();
        r.actor =
            json!({"principal_id":self.p.id,"role":self.p.role,"generation":self.p.generation});
        self.signer.seal(&mut r)?;
        self.put(r.clone())?;
        Ok(r)
    }
    /// Prepare a readable immutable Document with a canonical JSON manifest.
    fn snapshot(&mut self, kind: &str, title: &str, mut payload: Value) -> Result<Record> {
        let id = Uuid::new_v4().to_string();
        let content = format!(
            "# {title}\n\n```json\n{}\n```",
            serde_json::to_string_pretty(&payload).unwrap()
        );
        payload["document_id"] = json!(id);
        payload["content_hash"] = json!(content_hash(&content));
        self.plan.effects.push(Effect::CreateDocument{input:json!({"id":id,"title":title,"content":content,"projectId":self.s.config.payload["general_project_id"]})});
        self.add(kind, payload, None)
    }
    /// Pin a record's Document for read-back verification before preparing any mutation.
    fn verify_document(&mut self, r: &Record) {
        if r.payload["document_id"].is_string() && r.payload["content_hash"].is_string() {
            self.plan.documents.push(r.clone());
        }
    }
    /// Read and pin the current published plan, requiring its expected hash on plan-sensitive writes.
    fn current_plan(&mut self, args: &Value) -> Result<Record> {
        let id = text(&self.head.payload, "plan_record_id").map_err(|_| {
            Fault::new(
                "PLAN_REQUIRED",
                "Publish a plan before assigning or starting work",
            )
        })?;
        let r = self.s.record(id, Some("plan"))?.clone();
        require(
            args["expected"]["plan_hash"] == self.head.payload["plan_hash"],
            "STALE_PLAN",
            "Supply the current plan hash from context",
        )?;
        self.verify_document(&r);
        Ok(r)
    }
    /// Require the current executor assignment covering this work, including generation and recovery gates.
    fn executor(&self, args: &Value, recovering: bool) -> Result<Record> {
        let id = args["assignment_id"]
            .as_str()
            .or(self.p.assignment_id.as_deref())
            .or_else(|| {
                self.head.payload["assignment_ids"]
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(Value::as_str)
            })
            .ok_or_else(|| Fault::new("STALE_ASSIGNMENT", "Current assignment is required"))?;
        let a = self.s.record(id, Some("assignment"))?.clone();
        require(
            self.p.role.controls() || a.payload["principal_id"] == self.p.id,
            "UNAUTHORIZED",
            "Only the current executor may perform this action",
        )?;
        require(
            array(&a.payload, "scope_work_ids")
                .iter()
                .filter_map(Value::as_str)
                .any(|id| self.s.within(&self.work, id)),
            "OUT_OF_SCOPE",
            "Assignment does not cover work",
        )?;
        require(
            args["expected"]["assignment_generation"] == a.payload["generation"],
            "STALE_ASSIGNMENT",
            "Supply the current assignment generation",
        )?;
        require(
            if recovering {
                a.payload["status"] == "recovering" || a.payload["status"] == "needs_ack"
            } else {
                a.payload["status"] == "active"
            },
            "NOT_RECOVERED",
            "Assignment must complete recovery before continuing",
        )?;
        Ok(a)
    }
    /// Change only the managed native state; human labels and other fields remain intact.
    fn state(&mut self, state: &str) -> Result<()> {
        let state_id = self.s.config.payload["issue_state_ids"][state].clone();
        require(
            state_id.is_string(),
            "CONFIG_INVALID",
            "Workflow state mapping is missing",
        )?;
        self.head.payload["state"] = json!(state);
        self.head.payload["native_state_id"] = state_id.clone();
        self.plan.effects.push(Effect::UpdateIssue {
            id: self.work.clone(),
            input: json!({"stateId":state_id}),
        });
        if self.s.work(&self.work)?.identity.payload["kind"] == "epic" {
            let project = self.s.work(&self.work)?.identity.payload["epic_project_id"]
                .as_str()
                .unwrap_or("");
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
    /// Reject unresolved owner questions, gated dependencies and missing contract agreements.
    fn gates(&self, plan: &Record, stage: &str) -> Result<()> {
        require(
            self.head.payload["pending_scope_change"] != true,
            "PLAN_REQUIRED",
            "Publish the approved scope change before continuing",
        )?;
        for q in self
            .s
            .records
            .values()
            .filter(|r| r.record_kind == "question" && r.payload["status"] == "open")
        {
            require(
                !array(&q.payload, "blocked_work_ids")
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|id| self.s.within(&self.work, id)),
                "OWNER_DECISION_REQUIRED",
                "An owner question blocks this work",
            )?;
        }
        for dep in array(&plan.payload, "dependencies")
            .iter()
            .filter(|d| d["gate"] == stage)
        {
            let work = self.s.work(text(dep, "work_id")?)?;
            no_drift(self.s, &work.identity.work_id)?;
            let satisfied = match text(dep, "required_result")? {
                "task_local" => work.head.payload["acceptance_id"]
                    .as_str()
                    .is_some_and(|id| {
                        self.s
                            .record(id, Some("acceptance"))
                            .is_ok_and(|r| r.payload["level"] == "task_local")
                    }),
                "accepted" => work.head.payload["acceptance_id"]
                    .as_str()
                    .is_some_and(|id| {
                        self.s
                            .record(id, Some("acceptance"))
                            .is_ok_and(|r| r.payload["level"] != "task_local")
                    }),
                "submission" => work.head.payload["current_submission_id"].is_string(),
                "candidate" => work.head.payload["candidate_id"].is_string(),
                _ => false,
            };
            require(
                satisfied,
                "DEPENDENCY_PENDING",
                "A required dependency result is missing",
            )?;
        }
        for contract in array(&plan.payload, "contracts") {
            for party in array(contract, "parties") {
                let found = self
                    .s
                    .records
                    .values()
                    .filter(|r| {
                        r.record_kind == "contract_attestation"
                            && r.work_id == party.as_str().unwrap_or("")
                            && r.payload["contract_source"] == contract["source"]
                    })
                    .max_by_key(|r| r.created_at.clone());
                let att = found.ok_or_else(|| {
                    Fault::new(
                        "CONTRACT_UNAGREED",
                        "A contract party has not agreed to the pinned version",
                    )
                })?;
                require(
                    att.payload["agreement"] == "agree" || att.payload["agreement"] == "unchanged",
                    "CONTRACT_UNAGREED",
                    "Contract is disputed",
                )?;
                if contract["preparation_required"] == true {
                    require(
                        att.payload["preparation"] == "ready"
                            && att.payload["observation"].is_object(),
                        "PREPARATION_UNKNOWN",
                        "Required contract preparation is unobserved",
                    )?;
                }
            }
        }
        Ok(())
    }
    /// Require all immutable pins to exist and match their recorded hash.
    fn pins(&mut self, pins: &[Value]) -> Result<()> {
        for pin in pins {
            match text(pin, "kind")? {
                "linear_publication" => {
                    let r = self
                        .s
                        .record(text(pin, "id")?, Some("publication"))?
                        .clone();
                    require(
                        self.s.is_committed(&r),
                        "KNOWLEDGE_UNPUBLISHED",
                        "Pinned publication has not committed",
                    )?;
                    require(
                        r.payload["content_hash"] == pin["sha256"],
                        "STALE_PLAN",
                        "Publication pin changed",
                    )?;
                    self.verify_document(&r);
                }
                "repository_artifact" => {
                    for k in ["repository_id", "commit", "path"] {
                        text(pin, k)?;
                    }
                }
                _ => return Err(Fault::new("INVALID_PLAN", "Unsupported source kind")),
            }
        }
        Ok(())
    }
    /// Save new evidence without allowing an existing immutable ID to change.
    fn evidence(&mut self, items: &[Value]) -> Result<Vec<String>> {
        let mut ids = vec![];
        for e in items {
            let id = text(e, "id")?;
            require(
                !ids.iter().any(|existing| existing == id),
                "INVALID_INPUT",
                "Evidence IDs must be unique within a request",
            )?;
            if let Some(old) = self.s.records.get(id) {
                require(
                    old.record_kind == "evidence" && old.payload == *e,
                    "PAYLOAD_MISMATCH",
                    "Evidence ID is immutable",
                )?;
            } else {
                if !self.p.role.controls()
                    && let Some(producer) = e["producer_principal"].as_str()
                {
                    require(
                        producer == self.p.id,
                        "UNAUTHORIZED",
                        "Evidence producer must match the authenticated executor",
                    )?;
                }
                self.add("evidence", e.clone(), Some(id.into()))?;
            }
            ids.push(id.into());
        }
        Ok(ids)
    }
    /// Validate completion evidence against the exact result, criteria, reuse and required publications.
    fn proof(&mut self, args: &Value, plan: &Record, subject: &str) -> Result<Vec<String>> {
        let items = array(args, "evidence");
        let mut ids = self.evidence(items)?;
        let mut evidence = items.to_vec();
        for reuse in array(args, "reuse_evidence") {
            let old = self
                .s
                .record(text(reuse, "evidence_id")?, Some("evidence"))?;
            require(
                reuse["target_subject_hash"] == subject,
                "STALE_EVIDENCE",
                "Reused evidence targets another result",
            )?;
            if old.payload["subject_hash"] != subject {
                text(reuse, "reuse_reason").map_err(|_| {
                    Fault::new(
                        "STALE_EVIDENCE",
                        "Changed result needs explicit evidence applicability",
                    )
                })?;
            }
            ids.push(old.record_id.clone());
            let mut applicable = old.payload.clone();
            applicable["subject_hash"] = json!(subject);
            evidence.push(applicable);
        }
        for criterion in array(&plan.payload, "criteria")
            .iter()
            .filter(|c| c["required"] == true)
        {
            require(
                evidence.iter().any(|e| {
                    e["result"] == "passed"
                        && e["subject_hash"] == subject
                        && array(e, "criterion_ids").contains(&criterion["id"])
                        && e["primary_output"].is_object()
                }),
                "EVIDENCE_MISSING",
                format!(
                    "Required criterion {} lacks applicable passed evidence",
                    criterion["id"]
                ),
            )?;
        }
        if self.s.work(&self.work)?.identity.payload["classification"] == "bug" {
            require(
                (self.s.work(&self.work)?.identity.payload["kind"] != "task"
                    || !array(args, "bugs_reproduced").is_empty())
                    && evidence.iter().any(|e| {
                        e["kind"] == "reproduction"
                            && e["result"] == "passed"
                            && e["subject_hash"] == subject
                    }),
                "REPRODUCTION_REQUIRED",
                "Repeat the original bug reproduction and record its result",
            )?;
        }
        for out in array(&plan.payload, "knowledge_outputs")
            .iter()
            .filter(|o| o["required"] == true)
        {
            let result = array(args, "knowledge_results")
                .iter()
                .find(|r| r["criterion_id"] == out["criterion_id"])
                .ok_or_else(|| {
                    Fault::new(
                        "KNOWLEDGE_UNPUBLISHED",
                        "Required knowledge output is missing",
                    )
                })?;
            let publication = self
                .s
                .record(text(result, "publication_id")?, Some("publication"))?
                .clone();
            require(
                self.s.is_committed(&publication),
                "KNOWLEDGE_UNPUBLISHED",
                "Required knowledge publication has not committed",
            )?;
            require(
                publication.payload["content_hash"] == result["sha256"],
                "SNAPSHOT_TAMPERED",
                "Knowledge output hash mismatch",
            )?;
            let expected = out["basis_required"].as_str().unwrap_or("reported");
            let basis = publication.payload["basis"]
                .as_str()
                .unwrap_or("unclassified");
            require(
                match expected {
                    "owner_confirmed" => basis == "confirmed",
                    "verified" => matches!(basis, "verified" | "confirmed"),
                    _ => basis != "unclassified",
                },
                "KNOWLEDGE_UNPUBLISHED",
                "Knowledge output lacks required provenance",
            )?;
            self.verify_document(&publication);
        }
        Ok(ids)
    }
    /// Require mandatory children to carry the requested level of accepted result.
    fn child_results(&self, plan: &Record) -> Result<Vec<String>> {
        let mut ids = vec![];
        for child in array(&plan.payload, "mandatory_children") {
            let w = self.s.work(child.as_str().unwrap_or(""))?;
            no_drift(self.s, &w.identity.work_id)?;
            require(
                w.identity.payload["primary_parent_id"] == self.work,
                "INVALID_PLAN",
                "Mandatory child is not a direct child",
            )?;
            let id = w.head.payload["acceptance_id"]
                .as_str()
                .ok_or_else(|| Fault::new("EVIDENCE_MISSING", "Mandatory child is not accepted"))?;
            let result = self.s.record(id, Some("acceptance"))?;
            require(
                if w.identity.payload["kind"] == "task" {
                    result.payload["level"] == "task_local"
                } else {
                    result.payload["level"] != "task_local"
                },
                "EVIDENCE_MISSING",
                "Child acceptance has the wrong level",
            )?;
            ids.push(id.into());
        }
        Ok(ids)
    }
    /// Reject a still-active or unobserved writer before a transfer or terminal transition.
    fn stopped(&self) -> Result<()> {
        for a in self.s.records.values().filter(|r| {
            r.record_kind == "assignment"
                && (self.s.within(&r.work_id, &self.work) || self.s.within(&self.work, &r.work_id))
                && r.payload["status"] != "revoked"
        }) {
            if let Some(id) = a.payload["latest_attempt_id"].as_str() {
                let attempt = self.s.record(id, Some("attempt"))?;
                require(
                    attempt.payload["observation"]["writer_state"] == "stopped",
                    "WRITER_ACTIVE",
                    "Writer is active or its stop is unknown",
                )?;
            }
        }
        Ok(())
    }
    /// Finish by writing the signed head after native effects and immutable facts.
    fn finish(mut self, key: &str) -> Result<Plan> {
        self.head.revision += 1;
        self.head.payload["last_committed_operation"] = json!(key);
        self.head.created_at = crate::model::now();
        self.signer.seal(&mut self.head)?;
        self.plan.result.version = json!({"work_revision":self.head.revision});
        if self.head.payload["plan_hash"].is_string() {
            self.plan.result.version["plan_hash"] = self.head.payload["plan_hash"].clone();
        }
        if let Some(g) = self.p.generation {
            self.plan.result.version["assignment_generation"] = json!(g);
        }
        self.plan.result.status = "committed".into();
        self.plan.result.operation_key = Some(key.into());
        if self.plan.result.data["created_work_id"].is_string() {
            self.plan.result.data["parent_revision"] = json!(self.head.revision);
            self.plan.result.version = json!({"work_revision":1});
        } else {
            self.plan.result.data["work_id"] = json!(self.work);
        }
        self.plan.result.available_actions.push(json!({"tool":"at_context","work_id":self.plan.result.data["work_id"],"reason":"Read the committed work and current expected tokens","required_role":self.p.role.name(),"executable_now":true}));
        self.put(self.head.clone())?;
        Ok(self.plan)
    }
}

/// Resolve the revision-owning work for intents addressing records or knowledge.
pub fn target(s: &Snapshot, args: &Value) -> Result<String> {
    if let Some(id) = args["work_id"]
        .as_str()
        .or(args["parent_id"].as_str())
        .or(args["epic_id"].as_str())
    {
        return Ok(id.into());
    }
    if let Some(id) = args["record_id"].as_str() {
        return Ok(s.record(id, None)?.work_id.clone());
    }
    Ok(s.product.clone())
}

/// Build one fully guarded write plan without performing an external side effect.
pub fn plan(
    s: &Snapshot,
    p: &Principal,
    signer: &Signer,
    name: &str,
    args: &Value,
) -> Result<Plan> {
    let work = target(s, args)?;
    let key = text(args, "idempotency_key")?;
    // Integration facts and final runtime observations append without rewriting an accepted result.
    let append_to_terminal = matches!(name, "at_integration_record" | "at_execution_observe");
    let source = s;
    let mut b = Builder::new(source, p, signer, args, &work, append_to_terminal)?;
    match name {
        "at_work_create" => {
            let kind = text(args, "kind")?;
            let parent = source.work(&work)?;
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
            if b.head.payload["plan_record_id"].is_string() {
                b.stopped()?;
                require(
                    source.records.values().any(|r| {
                        r.record_kind == "change_proposal"
                            && r.work_id == work
                            && r.payload["status"] == "approved"
                            && r.payload["base_plan_hash"] == b.head.payload["plan_hash"]
                            && array(&r.payload, "affected_work_ids").contains(&json!(work))
                    }),
                    "PARENT_SCOPE_FROZEN",
                    "Approve a scope-change proposal for this parent before adding work",
                )?;
                b.head.payload["pending_scope_change"] = json!(true);
            }
            if pk == "task" {
                require(
                    source.config.payload["policy"]["nested_atomic_verified"] == true,
                    "SCHEMA_UNSUPPORTED",
                    "Atomic-under-task requires live feasibility verification",
                )?;
            }
            let id = Uuid::new_v4().to_string();
            let general = source.config.payload["general_project_id"].clone();
            let mut project = if pk == "epic" {
                parent.identity.payload["epic_project_id"].clone()
            } else {
                parent.identity.payload["native_project_id"].clone()
            };
            let epic_project = if kind == "epic" {
                let pid = Uuid::new_v4().to_string();
                b.plan.effects.push(Effect::CreateProject{input:json!({"id":pid,"name":args["title"],"content":args["description"],"teamIds":source.config.payload["team_ids"]})});
                b.plan.effects.push(Effect::LinkProject{input:json!({"id":Uuid::new_v4().to_string(),"initiativeId":source.config.payload["initiative_id"],"projectId":pid})});
                project = general;
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
            let state = source.config.payload["issue_state_ids"]["draft"].clone();
            let label_key = if kind == "epic" {
                "epic_companion"
            } else {
                kind
            };
            let labels = vec![
                source.config.payload["kind_label_ids"][label_key].clone(),
                source.config.payload["managed_label_id"].clone(),
            ]
            .into_iter()
            .filter(Value::is_string)
            .collect::<Vec<_>>();
            b.plan.effects.push(Effect::CreateIssue{input:json!({"id":id,"title":args["title"],"description":args["description"],"teamId":source.config.payload["team_ids"][0],"projectId":project,"parentId":native_parent,"stateId":state,"labelIds":labels})});
            let identity=signer.record(p,&source.product,&id,"identity",json!({"kind":kind,"primary_parent_id":work,"native_issue_id":id,"native_project_id":project,"native_parent_id":native_parent,"epic_project_id":epic_project,"record_base_url":"$created_issue_url","classification":args["classification"].as_str().unwrap_or("feature"),"mandatory":args["mandatory"].as_bool().unwrap_or(true),"replacement_for":args["replacement_for"]}),None)?;
            let head = signer.record(
                p,
                &source.product,
                &id,
                "work_head",
                json!({"state":"draft","native_state_id":state,"children":[],"assignment_ids":[]}),
                None,
            )?;
            b.plan.effects.push(Effect::NewWork {
                identity,
                head: Box::new(head),
            });
            let mut children = array(&b.head.payload, "children").to_vec();
            children.push(json!(id));
            b.head.payload["children"] = json!(children);
            b.plan.result.data = json!({"work_id":id,"created_work_id":id,"native_issue_id":id,"kind":kind,"native_project_id":epic_project.clone().map(Value::String).unwrap_or(project),"state":"draft","revision":1});
        }
        "at_plan_publish" => {
            let criteria = array(args, "criteria");
            let ids: Vec<_> = criteria
                .iter()
                .map(|c| text(c, "id"))
                .collect::<Result<_>>()?;
            require(
                ids.iter().copied().collect::<BTreeSet<_>>().len() == ids.len(),
                "INVALID_PLAN",
                "Criterion IDs must be unique",
            )?;
            let mut seen = BTreeSet::new();
            for dep in array(args, "dependencies") {
                let id = text(dep, "work_id")?;
                source.work(id)?;
                require(
                    id != work && !depends_on(source, id, &work, &mut seen),
                    "DEPENDENCY_CYCLE",
                    "Dependency graph would contain a cycle",
                )?;
            }
            let mut pins = array(args, "inputs").to_vec();
            pins.extend(array(args, "contracts").iter().map(|c| c["source"].clone()));
            b.pins(&pins)?;
            let children: BTreeSet<_> = array(args, "mandatory_children")
                .iter()
                .filter_map(Value::as_str)
                .collect();
            let actual: BTreeSet<_> = source
                .works
                .values()
                .filter(|w| {
                    w.identity.payload["primary_parent_id"] == work
                        && w.identity.payload["mandatory"] != false
                })
                .map(|w| w.identity.work_id.as_str())
                .collect();
            require(
                children == actual,
                "INVALID_PLAN",
                "Plan must include exactly the mandatory direct children",
            )?;
            if b.head.payload["plan_record_id"].is_string() {
                require(
                    args["expected"]["plan_hash"] == b.head.payload["plan_hash"],
                    "STALE_PLAN",
                    "Plan replacement requires the current plan hash",
                )?;
                b.stopped()?;
                let proposal =
                    source.record(text(args, "change_proposal_id")?, Some("change_proposal"))?;
                require(
                    proposal.work_id == work
                        && proposal.payload["base_plan_hash"] == b.head.payload["plan_hash"],
                    "STALE_PLAN",
                    "Proposal does not target the current plan",
                )?;
                let decision = source.record(text(args, "owner_decision_id")?, Some("decision"))?;
                require(
                    decision.payload["target_record_id"] == proposal.record_id
                        && decision.payload["decision_key"] == "approve",
                    "OWNER_DECISION_REQUIRED",
                    "Plan changes require approval of this proposal",
                )?;
                let mut approved = proposal.payload.clone();
                approved["status"] = json!("applied");
                b.update(proposal.clone(), approved)?;
            }
            let mut payload = args.clone();
            for k in ["product_id", "idempotency_key", "expected", "work_id"] {
                payload.as_object_mut().unwrap().remove(k);
            }
            payload["plan_version"] =
                json!(b.head.payload["plan_version"].as_u64().unwrap_or(0) + 1);
            let plan_hash = hash(&payload)?;
            payload["plan_hash"] = json!(plan_hash);
            let plan = b.snapshot(
                "plan",
                &format!(
                    "Plan · {}",
                    source.work(&work)?.native["title"]
                        .as_str()
                        .unwrap_or("work")
                ),
                payload,
            )?;
            b.head.payload["plan_record_id"] = json!(plan.record_id);
            b.head.payload["plan_hash"] = json!(plan_hash);
            b.head.payload["plan_version"] = plan.payload["plan_version"].clone();
            b.head.payload["pending_scope_change"] = json!(false);
            for id in array(&b.head.payload, "assignment_ids").to_vec() {
                let a = source
                    .record(id.as_str().unwrap_or(""), Some("assignment"))?
                    .clone();
                let mut payload = a.payload.clone();
                payload["status"] = json!("needs_ack");
                b.update(a, payload)?;
            }
            b.state("ready")?;
            b.plan.result.data = json!({"plan_id":plan.record_id,"plan_hash":plan_hash,"plan_document_id":plan.payload["document_id"],"plan_revision":plan.payload["plan_version"]});
        }
        "at_contract_confirm" => {
            let plan = b.current_plan(args)?;
            require(
                array(&plan.payload, "contracts")
                    .iter()
                    .any(|c| c["source"] == args["contract_source"]),
                "INVALID_PLAN",
                "Contract is not pinned by this plan",
            )?;
            if !p.role.controls() {
                b.executor(args, false)?;
            } else {
                text(args, "attributed_principal").map_err(|_| {
                    Fault::new(
                        "UNAUTHORIZED",
                        "Root must attribute the contract statement to its actual principal",
                    )
                })?;
                require(
                    args["observation"].is_object(),
                    "EVIDENCE_MISSING",
                    "Attributed agreement requires its source",
                )?;
            }
            require(
                args["preparation"] != "ready" || args["observation"].is_object(),
                "PREPARATION_UNKNOWN",
                "Ready preparation requires an observation",
            )?;
            let r = b.add("contract_attestation", args.clone(), None)?;
            b.plan.result.data = json!({"attestation_id":r.record_id});
        }
        "at_assign" => {
            let plan = b.current_plan(args)?;
            let who = text(args, "principal_id")?;
            let role = text(args, "role")?;
            for id in array(&b.head.payload, "assignment_ids") {
                let existing = source.record(id.as_str().unwrap_or(""), Some("assignment"))?;
                if existing.payload["status"] != "revoked" && existing.payload["role"] == role {
                    require(
                        existing.payload["principal_id"] == who,
                        "ASSIGNMENT_EXISTS",
                        "Use transfer to replace responsibility",
                    )?;
                    let mut out = Plan {
                        target_work_id: work.clone(),
                        effects: vec![],
                        documents: vec![plan],
                        result: Outcome::ok(
                            json!({"assignment_id":existing.record_id,"generation":existing.payload["generation"]}),
                        ),
                        actor: p.clone(),
                    };
                    out.result.status = "noop".into();
                    out.result.operation_key = Some(key.into());
                    out.result.data["work_id"] = json!(work);
                    out.result.version = json!({"work_revision":b.head.revision,"plan_hash":b.head.payload["plan_hash"]});
                    return Ok(out);
                }
            }
            let assignment=b.add("assignment",json!({"principal_id":who,"role":role,"scope_work_ids":[work],"scope_text":args["scope"],"generation":1,"plan_hash":b.head.payload["plan_hash"],"pinned_inputs":plan.payload["inputs"],"status":"active","attempt_ids":[],"workspace_ref":args["workspace_ref"]}),None)?;
            let mut ids = array(&b.head.payload, "assignment_ids").to_vec();
            ids.push(json!(assignment.record_id));
            b.head.payload["assignment_ids"] = json!(ids);
            let mut update = json!({});
            if let Some(id) = args["human_assignee_id"].as_str() {
                update["assigneeId"] = json!(id)
            }
            if let Some(id) = args["delegate_app_user_id"].as_str() {
                update["delegateId"] = json!(id)
            }
            if !update.as_object().unwrap().is_empty() {
                b.plan.effects.push(Effect::UpdateIssue {
                    id: work.clone(),
                    input: update,
                });
            }
            b.plan.result.data = json!({"assignment_id":assignment.record_id,"generation":1,"principal_id":who,"role":role,"scope_work_ids":[work]});
        }
        "at_execution_observe" => {
            b.current_plan(args)?;
            let assignment = b.executor(args, false)?;
            text(&args["observation"], "run_id")?;
            text(&args["observation"]["source"], "locator")?;
            let record=b.add("attempt",json!({"assignment_id":assignment.record_id,"generation":assignment.payload["generation"],"observation":args["observation"]}),None)?;
            let mut payload = assignment.payload.clone();
            let mut ids = array(&payload, "attempt_ids").to_vec();
            ids.push(json!(record.record_id));
            payload["attempt_ids"] = json!(ids);
            payload["latest_attempt_id"] = json!(record.record_id);
            if args["observation"]["writer_state"] == "stopped" {
                for permit in source.records.values().filter(|r| {
                    r.record_kind == "writer_permit"
                        && r.payload["assignment_id"] == assignment.record_id
                        && r.payload["status"] == "active"
                }) {
                    let mut payload = permit.payload.clone();
                    payload["status"] = json!("released");
                    payload["stop_observation_id"] = json!(record.record_id);
                    b.update(permit.clone(), payload)?;
                }
            }
            b.update(assignment, payload)?;
            b.plan.result.data = json!({"attempt_id":record.record_id,"writer_state":args["observation"]["writer_state"]});
        }
        "at_begin" => {
            let plan = b.current_plan(args)?;
            let assignment = b.executor(args, false)?;
            require(
                assignment.payload["plan_hash"]
                    == source.work(&assignment.work_id)?.head.payload["plan_hash"],
                "PLAN_ACK_REQUIRED",
                "Assignment has not acknowledged this plan",
            )?;
            require(
                b.head.payload["state"] == "ready" || b.head.payload["state"] == "in_progress",
                "PLAN_REQUIRED",
                "Work is not ready to begin",
            )?;
            b.gates(&plan, "start")?;
            let attempt = source.record(text(args, "attempt_id")?, Some("attempt"))?;
            require(
                assignment.payload["latest_attempt_id"] == attempt.record_id,
                "PREPARATION_UNKNOWN",
                "Begin requires the latest runtime observation",
            )?;
            require(
                attempt.payload["assignment_id"] == assignment.record_id
                    && attempt.payload["generation"] == assignment.payload["generation"],
                "STALE_ASSIGNMENT",
                "Attempt belongs to another assignment generation",
            )?;
            require(
                attempt.payload["observation"]["state"] == "running",
                "PREPARATION_UNKNOWN",
                "A current running runtime observation is required",
            )?;
            let workspace = args["workspace_ref"]
                .as_str()
                .or(assignment.payload["workspace_ref"].as_str());
            if code_work(source, &work)? {
                require(
                    workspace.is_some() && args["prepared_source"].is_object(),
                    "PREPARATION_UNKNOWN",
                    "Code work requires a prepared workspace observation",
                )?;
            }
            if let Some(ws) = workspace {
                for permit in source.records.values().filter(|r| {
                    r.record_kind == "writer_permit"
                        && r.payload["workspace_ref"] == ws
                        && r.payload["status"] == "active"
                }) {
                    require(
                        permit.payload["assignment_id"] == assignment.record_id,
                        "WRITER_ACTIVE",
                        "Another assignment holds this workspace",
                    )?;
                }
                let permit=b.add("writer_permit",json!({"assignment_id":assignment.record_id,"generation":assignment.payload["generation"],"workspace_ref":ws,"scope_work_ids":[work],"status":"active","attempt_id":attempt.record_id}),None)?;
                b.head.payload["writer_permit_id"] = json!(permit.record_id);
            }
            let begin=b.add("begin",json!({"assignment_id":assignment.record_id,"attempt_id":attempt.record_id,"plan_hash":b.head.payload["plan_hash"],"prepared_source":args["prepared_source"]}),None)?;
            b.head.payload["begin_id"] = json!(begin.record_id);
            b.state("in_progress")?;
            b.plan.result.data = json!({"begin_id":begin.record_id,"writer_permit_id":b.head.payload["writer_permit_id"]});
        }
        "at_checkpoint" => {
            b.current_plan(args)?;
            if !matches!(p.role, Role::Decomposer) {
                b.executor(args, false)?;
            }
            let ids = b.evidence(array(args, "evidence"))?;
            for response in array(args, "finding_responses") {
                source.record(text(response, "finding_id")?, Some("finding"))?;
            }
            let comment = Uuid::new_v4().to_string();
            b.plan.effects.push(Effect::CreateComment{input:json!({"id":comment,"issueId":work,"body":format!("{}\n\nRemaining: {}",text(args,"summary")?,args["remaining"].as_str().unwrap_or(""))})});
            let r=b.add("checkpoint",json!({"summary":args["summary"],"remaining":args["remaining"],"artifacts":args["artifacts"],"evidence_ids":ids,"finding_responses":args["finding_responses"],"comment_id":comment}),None)?;
            b.head.payload["checkpoint_id"] = json!(r.record_id);
            b.plan.result.data = json!({"checkpoint_id":r.record_id});
        }
        "at_task_complete" | "at_submit" => {
            let plan = b.current_plan(args)?;
            let kind = source.work(&work)?.identity.payload["kind"]
                .as_str()
                .unwrap_or("");
            require(
                if name == "at_task_complete" {
                    kind == "task"
                } else {
                    matches!(kind, "module" | "atomic" | "epic")
                },
                "INVALID_PARENT",
                "Wrong work level for this completion tool",
            )?;
            if kind != "epic" {
                b.executor(args, false)?;
            }
            require(
                b.head.payload["begin_id"].is_string() || kind == "epic",
                "PREPARATION_UNKNOWN",
                "Work has not begun",
            )?;
            b.gates(&plan, "submission")?;
            let subject = hash(&args["artifacts"])?;
            if code_work(source, &work)? {
                require(
                    array(args, "artifacts").iter().any(|a| {
                        a["kind"] == "git_commit"
                            && a["commit"].as_str().is_some_and(|c| c.len() >= 7)
                    }),
                    "EVIDENCE_MISSING",
                    "Code results require an identified Git commit",
                )?;
            }
            let evidence = b.proof(args, &plan, &subject)?;
            let children = b.child_results(&plan)?;
            if kind == "epic" {
                let candidate = source.record(text(args, "candidate_id")?, Some("candidate"))?;
                require(
                    candidate.work_id == work
                        && candidate.payload["kind"] == "final"
                        && candidate.payload["computed_complete"] == true
                        && candidate.payload["subject_hash"] == subject,
                    "INCOMPLETE_CANDIDATE",
                    "Epic requires a complete final candidate",
                )?;
            }
            if name == "at_task_complete" {
                let r=b.add("acceptance",json!({"level":"task_local","subject_hash":subject,"plan_hash":b.head.payload["plan_hash"],"artifacts":args["artifacts"],"evidence_ids":evidence,"mandatory_child_acceptance_ids":children,"knowledge_results":args["knowledge_results"],"reason":args["result_summary"]}),None)?;
                b.head.payload["acceptance_id"] = json!(r.record_id);
                b.state("accepted")?;
                b.plan.result.data = json!({"acceptance_id":r.record_id,"acceptance_level":"task_local","subject_hash":subject});
            } else {
                let r=b.snapshot("submission","Submitted result",json!({"subject_hash":subject,"plan_id":plan.record_id,"plan_hash":b.head.payload["plan_hash"],"summary":args["summary"],"artifacts":args["artifacts"],"evidence_ids":evidence,"child_result_ids":children,"knowledge_results":args["knowledge_results"],"candidate_id":args["candidate_id"]}))?;
                b.head.payload["current_submission_id"] = json!(r.record_id);
                b.state("review")?;
                b.plan.result.data = json!({"submission_id":r.record_id,"subject_hash":subject,"document_id":r.payload["document_id"]});
            }
        }
        "at_review_open" => {
            let plan = b.current_plan(args)?;
            let submission = source
                .record(text(args, "submission_id")?, Some("submission"))?
                .clone();
            require(
                b.head.payload["current_submission_id"] == submission.record_id,
                "STALE_SUBMISSION",
                "Review must address the current submission",
            )?;
            b.verify_document(&submission);
            let reviewer = text(args, "reviewer_principal")?;
            require(
                submission.actor["principal_id"] != reviewer
                    && !source.records.values().any(|r| {
                        r.record_kind == "assignment"
                            && r.work_id == work
                            && matches!(r.payload["role"].as_str(), Some("lead" | "helper"))
                            && r.payload["principal_id"] == reviewer
                    }),
                "NOT_INDEPENDENT",
                "Reviewer also implemented this work",
            )?;
            let required: BTreeSet<_> = array(&plan.payload, "criteria")
                .iter()
                .filter(|c| c["required"] == true)
                .map(|c| c["id"].clone().to_string())
                .collect();
            let supplied: BTreeSet<_> = array(args, "required_criteria")
                .iter()
                .map(Value::to_string)
                .collect();
            require(
                required.is_subset(&supplied),
                "REVIEW_INCOMPLETE",
                "Review scope omits mandatory criteria",
            )?;
            let assignment = if let Some(case_id) = b.head.payload["review_case_id"].as_str() {
                let case = source.record(case_id, Some("review_case"))?;
                let current = source
                    .record(
                        text(&case.payload, "reviewer_assignment_id")?,
                        Some("assignment"),
                    )?
                    .clone();
                require(
                    current.payload["principal_id"] == reviewer
                        && current.payload["status"] == "active",
                    "ASSIGNMENT_EXISTS",
                    "Use transfer and recovery to replace the reviewer",
                )?;
                current
            } else {
                b.add("assignment",json!({"principal_id":reviewer,"role":if args["review_kind"]=="composition"{"integrator"}else{"reviewer"},"scope_work_ids":[work],"scope_text":args["scope"],"generation":1,"plan_hash":b.head.payload["plan_hash"],"status":"active","pinned_inputs":plan.payload["inputs"],"attempt_ids":[]}),None)?
            };
            let payload = if let Some(id) = b.head.payload["review_case_id"].as_str() {
                let old = source.record(id, Some("review_case"))?;
                let mut payload = old.payload.clone();
                payload["reviewer_assignment_id"] = json!(assignment.record_id);
                payload["current_submission_id"] = json!(submission.record_id);
                payload["status"] = json!("open");
                payload["required_criteria"] = args["required_criteria"].clone();
                b.update(old.clone(), payload)?
            } else {
                b.add("review_case",json!({"case_kind":args["review_kind"],"reviewer_assignment_id":assignment.record_id,"current_submission_id":submission.record_id,"required_criteria":args["required_criteria"],"round_ids":[],"finding_ids":[],"status":"open"}),None)?
            };
            b.head.payload["review_case_id"] = json!(payload.record_id);
            b.plan.result.data = json!({"case_id":payload.record_id,"reviewer_assignment_id":assignment.record_id,"generation":assignment.payload["generation"]});
        }
        "at_review_report" => {
            review_report(&mut b, args)?;
        }
        "at_accept" => {
            accept(&mut b, args)?;
        }
        "at_knowledge_save" | "at_knowledge_publish" => {
            knowledge(&mut b, name, args)?;
        }
        "at_question_ask" => {
            let options = array(args, "options");
            let keys: Vec<_> = options
                .iter()
                .map(|o| text(o, "key"))
                .collect::<Result<_>>()?;
            require(
                keys.iter().copied().collect::<BTreeSet<_>>().len() == keys.len(),
                "INVALID_INPUT",
                "Question option keys must be unique",
            )?;
            if let Some(key) = args["recommended_key"].as_str() {
                require(
                    keys.contains(&key),
                    "INVALID_INPUT",
                    "Recommendation is not one of the options",
                )?;
            }
            for id in array(args, "blocked_work_ids") {
                in_scope(source, p, id.as_str().unwrap_or(""))?;
            }
            let token = Uuid::new_v4().to_string();
            let comment = Uuid::new_v4().to_string();
            b.plan.effects.push(Effect::CreateComment{input:json!({"id":comment,"issueId":work,"body":format!("{}\n\nOptions: {}\nDecision token: {}",text(args,"question")?,serde_json::to_string(options).unwrap(),token)})});
            let mut payload = args.clone();
            payload["decision_token"] = json!(token);
            payload["comment_id"] = json!(comment);
            payload["status"] = json!("open");
            let r = b.add("question", payload, None)?;
            b.plan.result.data = json!({"question_id":r.record_id,"decision_token":token});
        }
        "at_owner_decide" => {
            owner_decide(&mut b, args)?;
        }
        "at_change_propose" => {
            b.current_plan(args)?;
            require(
                args["base_plan_hash"] == b.head.payload["plan_hash"],
                "STALE_PLAN",
                "Proposal base plan changed",
            )?;
            for id in array(args, "affected_work_ids") {
                in_scope(source, p, id.as_str().unwrap_or(""))?;
            }
            let mut payload = args.clone();
            payload["status"] = json!("proposed");
            payload["requires_owner"] = json!(true);
            let r = b.snapshot("change_proposal", "Proposed scope change", payload)?;
            b.plan.result.data = json!({"proposal_id":r.record_id});
        }
        "at_candidate_register" => {
            candidate(&mut b, args)?;
        }
        "at_integration_record" => {
            let accepted = source.record(text(args, "acceptance_id")?, Some("acceptance"))?;
            require(
                accepted.work_id == work,
                "OUT_OF_SCOPE",
                "Acceptance belongs to another work",
            )?;
            require(
                args["evidence"]["result"] == "passed" && args["evidence"]["kind"] == "integration",
                "EVIDENCE_MISSING",
                "Integration needs passed integration evidence",
            )?;
            let ids = b.evidence(&[args["evidence"].clone()])?;
            let r=b.add("integration",json!({"acceptance_id":accepted.record_id,"target":args["target"],"method":args["method"],"evidence_id":ids[0]}),None)?;
            b.head.payload["state"] = s.work(&work)?.head.payload["state"].clone();
            b.plan.result.data = json!({"integration_id":r.record_id});
        }
        "at_transfer" => {
            transfer(&mut b, args)?;
        }
        "at_recovery_report" => {
            b.current_plan(args)?;
            let assignment = b.executor(args, true)?;
            require(
                assignment.payload["principal_id"] == p.id,
                "UNAUTHORIZED",
                "Recovery must be reported by the new assignee",
            )?;
            require(
                args["understood_plan_hash"] == b.head.payload["plan_hash"],
                "STALE_PLAN",
                "Recovery report must acknowledge the current plan",
            )?;
            let r = b.add("recovery_report", args.clone(), None)?;
            b.plan.result.data = json!({"recovery_report_id":r.record_id});
        }
        "at_recovery_confirm" => {
            b.current_plan(args)?;
            let assignment = b.executor(args, true)?;
            let report =
                source.record(text(args, "recovery_report_id")?, Some("recovery_report"))?;
            require(
                report.payload["assignment_id"] == assignment.record_id
                    && report.actor["principal_id"] == assignment.payload["principal_id"]
                    && report.actor["principal_id"] != p.id
                    && report.payload["understood_plan_hash"] == b.head.payload["plan_hash"],
                "NOT_RECOVERED",
                "Recovery report identity or plan is invalid",
            )?;
            b.stopped()?;
            let mut payload = assignment.payload.clone();
            payload["status"] = json!("active");
            payload["plan_hash"] = b.head.payload["plan_hash"].clone();
            b.update(assignment, payload)?;
            let r = b.add("recovery_confirmation", args.clone(), None)?;
            b.plan.result.data = json!({"confirmation_id":r.record_id});
        }
        "at_work_retire" => {
            b.stopped()?;
            let parent = source.work(&work)?.identity.payload["primary_parent_id"].as_str();
            if let Some(parent) = parent
                && source.work(parent)?.head.payload["plan_record_id"].is_string()
                && source.work(&work)?.identity.payload["mandatory"] != false
            {
                let decision = source.record(text(args, "decision_id")?, Some("decision"))?;
                require(
                    decision.payload["decision_key"] == "approve",
                    "OWNER_DECISION_REQUIRED",
                    "Retiring a mandatory child requires an approving decision",
                )?;
            }
            let r = b.add("retirement", args.clone(), None)?;
            b.state(text(args, "disposition")?)?;
            b.plan.result.data = json!({"retirement_id":r.record_id});
        }
        _ => {
            return Err(Fault::new(
                "UNKNOWN_TOOL",
                "No workflow recipe for this intent",
            ));
        }
    }
    if append_to_terminal {
        b.head.payload["state"] = s.work(&work)?.head.payload["state"].clone();
    }
    b.finish(key)
}

/// Identify code work conservatively from its declared classification.
fn code_work(s: &Snapshot, id: &str) -> Result<bool> {
    Ok(matches!(
        s.work(id)?.identity.payload["classification"].as_str(),
        Some("feature" | "bug" | "refactor" | "infrastructure")
    ))
}
/// Detect a path back to the proposed dependency source in current plans.
fn depends_on(s: &Snapshot, id: &str, target: &str, seen: &mut BTreeSet<String>) -> bool {
    if id == target {
        return true;
    }
    if !seen.insert(id.into()) {
        return false;
    }
    let Some(work) = s.works.get(id) else {
        return false;
    };
    let Some(plan) = work.head.payload["plan_record_id"]
        .as_str()
        .and_then(|id| s.records.get(id))
    else {
        return false;
    };
    array(&plan.payload, "dependencies")
        .iter()
        .filter_map(|d| d["work_id"].as_str())
        .any(|id| depends_on(s, id, target, seen))
}

/// Save reviewer evidence and retain every unresolved finding across rounds.
fn review_report(b: &mut Builder<'_>, args: &Value) -> Result<()> {
    b.current_plan(args)?;
    let case =
        b.s.record(text(args, "case_id")?, Some("review_case"))?
            .clone();
    require(
        case.work_id == b.work
            && case.payload["current_submission_id"] == args["submission_id"]
            && b.head.payload["current_submission_id"] == args["submission_id"],
        "STALE_SUBMISSION",
        "Review case does not target the current submission",
    )?;
    let assignment = b.s.record(
        text(&case.payload, "reviewer_assignment_id")?,
        Some("assignment"),
    )?;
    require(
        assignment.payload["principal_id"] == b.p.id
            && Some(assignment.record_id.as_str()) == b.p.assignment_id.as_deref(),
        "NOT_INDEPENDENT",
        "Only the assigned reviewer can record this round",
    )?;
    let ids = b.evidence(array(args, "evidence"))?;
    let mut findings = array(&case.payload, "finding_ids").to_vec();
    for finding in array(args, "findings") {
        let id = text(finding, "id")?;
        let previous = b.s.records.get(id);
        if let Some(previous) = previous {
            require(
                previous.record_kind == "finding"
                    && previous.work_id == b.work
                    && findings.contains(&json!(id)),
                "OUT_OF_SCOPE",
                "Finding is outside this review case",
            )?;
        } else {
            require(
                finding["state"] == "open",
                "EVIDENCE_MISSING",
                "New findings start open",
            )?;
        }
        if matches!(
            finding["state"].as_str(),
            Some("verified_fixed" | "verified_invalid")
        ) {
            require(
                !array(finding, "verification_evidence_ids").is_empty(),
                "EVIDENCE_MISSING",
                "Finding resolution requires verification evidence",
            )?;
            for eid in array(finding, "verification_evidence_ids") {
                let e = array(args, "evidence")
                    .iter()
                    .find(|e| e["id"] == *eid)
                    .or_else(|| {
                        b.s.records
                            .get(eid.as_str().unwrap_or(""))
                            .filter(|r| r.record_kind == "evidence")
                            .map(|r| &r.payload)
                    })
                    .ok_or_else(|| Fault::new("EVIDENCE_MISSING", "Finding evidence is missing"))?;
                require(
                    e["result"] == "passed",
                    "EVIDENCE_MISSING",
                    "Finding verification did not pass",
                )?;
            }
        }
        if finding["state"] == "accepted_risk" {
            let decision =
                b.s.record(text(finding, "owner_decision_id")?, Some("decision"))?;
            require(
                decision.payload["target_record_id"] == id
                    && decision.payload["decision_key"] == "accept_risk",
                "OWNER_DECISION_REQUIRED",
                "Risk acceptance must name this finding",
            )?;
        }
        if let Some(previous) = previous {
            b.update(previous.clone(), finding.clone())?;
        } else {
            b.add("finding", finding.clone(), Some(id.into()))?;
            findings.push(json!(id));
        }
    }
    for coverage in array(args, "coverage")
        .iter()
        .filter(|c| c["state"] == "covered")
    {
        require(
            !array(coverage, "evidence_ids").is_empty(),
            "REVIEW_INCOMPLETE",
            "Covered criteria need evidence",
        )?;
        for eid in array(coverage, "evidence_ids") {
            let submission =
                b.s.record(text(args, "submission_id")?, Some("submission"))?;
            let evidence = array(args, "evidence")
                .iter()
                .find(|e| e["id"] == *eid)
                .or_else(|| {
                    b.s.records
                        .get(eid.as_str().unwrap_or(""))
                        .filter(|r| r.record_kind == "evidence")
                        .map(|r| &r.payload)
                });
            require(
                evidence.is_some_and(|e| {
                    e["result"] == "passed"
                        && e["subject_hash"] == submission.payload["subject_hash"]
                        && array(e, "criterion_ids").contains(&coverage["criterion_id"])
                }),
                "EVIDENCE_MISSING",
                "Coverage requires passed evidence for this criterion and exact submission",
            )?;
        }
    }
    let round=b.snapshot("review_round","Independent review",json!({"case_id":case.record_id,"submission_id":args["submission_id"],"coverage":args["coverage"],"finding_ids":findings,"evidence_ids":ids,"summary":args["summary"],"reviewed_delta":args["reviewed_delta"],"limitations":args["limitations"]}))?;
    let mut payload = case.payload.clone();
    let mut rounds = array(&payload, "round_ids").to_vec();
    rounds.push(json!(round.record_id));
    payload["round_ids"] = json!(rounds);
    payload["finding_ids"] = json!(findings);
    payload["status"] = json!("open");
    b.update(case, payload)?;
    b.plan.result.data = json!({"review_round_id":round.record_id,"finding_ids":findings});
    Ok(())
}

/// Enforce independent review and exact composition before producing an acceptance certificate.
fn accept(b: &mut Builder<'_>, args: &Value) -> Result<()> {
    let plan = b.current_plan(args)?;
    b.gates(&plan, "acceptance")?;
    let submission =
        b.s.record(text(args, "submission_id")?, Some("submission"))?
            .clone();
    require(
        submission.work_id == b.work
            && b.head.payload["current_submission_id"] == submission.record_id
            && submission.payload["plan_hash"] == b.head.payload["plan_hash"],
        "STALE_SUBMISSION",
        "Acceptance must target the current plan and submission",
    )?;
    b.verify_document(&submission);
    let children = b.child_results(&plan)?;
    let case_id = text(args, "case_id").map_err(|_| {
        Fault::new(
            "REVIEW_INCOMPLETE",
            "Independent review is required for this result",
        )
    })?;
    let case = b.s.record(case_id, Some("review_case"))?;
    require(
        case.work_id == b.work
            && case.payload["current_submission_id"] == submission.record_id
            && b.head.payload["review_case_id"] == case_id,
        "STALE_SUBMISSION",
        "Review case is not current",
    )?;
    let round_id = array(&case.payload, "round_ids")
        .last()
        .and_then(Value::as_str)
        .ok_or_else(|| Fault::new("REVIEW_INCOMPLETE", "No review round exists"))?;
    let round = b.s.record(round_id, Some("review_round"))?.clone();
    require(
        round.payload["submission_id"] == submission.record_id,
        "REVIEW_INCOMPLETE",
        "Review does not cover the current submission",
    )?;
    require(
        round.actor["principal_id"] != submission.actor["principal_id"],
        "NOT_INDEPENDENT",
        "Review author is also submission author",
    )?;
    b.verify_document(&round);
    for criterion in array(&plan.payload, "criteria")
        .iter()
        .filter(|c| c["required"] == true)
    {
        require(
            array(&round.payload, "coverage").iter().any(|c| {
                c["criterion_id"] == criterion["id"]
                    && c["state"] == "covered"
                    && !array(c, "evidence_ids").is_empty()
            }),
            "REVIEW_INCOMPLETE",
            "Mandatory review coverage is incomplete",
        )?;
    }
    for id in array(&case.payload, "finding_ids") {
        let finding = b.s.record(id.as_str().unwrap_or(""), Some("finding"))?;
        require(
            finding.payload["state"] != "open" || finding.payload["severity"] == "suggestion",
            "FINDINGS_OPEN",
            "Unresolved review findings remain",
        )?;
    }
    for coverage in array(&round.payload, "coverage")
        .iter()
        .filter(|c| c["state"] == "covered")
    {
        for id in array(coverage, "evidence_ids") {
            let evidence = b.s.record(id.as_str().unwrap_or(""), Some("evidence"))?;
            require(
                evidence.payload["result"] == "passed"
                    && evidence.payload["subject_hash"] == submission.payload["subject_hash"]
                    && array(&evidence.payload, "criterion_ids")
                        .contains(&coverage["criterion_id"]),
                "STALE_EVIDENCE",
                "Review evidence does not support the exact accepted result",
            )?;
        }
    }
    let kind = b.s.work(&b.work)?.identity.payload["kind"].clone();
    if kind == "epic" {
        let candidate = b.s.record(
            text(&submission.payload, "candidate_id")?,
            Some("candidate"),
        )?;
        require(
            candidate.payload["kind"] == "final"
                && candidate.payload["computed_complete"] == true
                && candidate.payload["accepted_module_result_ids"] == json!(children)
                && candidate.payload["subject_hash"] == submission.payload["subject_hash"],
            "INCOMPLETE_CANDIDATE",
            "Final candidate differs from current accepted child versions",
        )?;
    }
    let r=b.add("acceptance",json!({"level":kind,"subject_hash":submission.payload["subject_hash"],"plan_hash":b.head.payload["plan_hash"],"submission_id":submission.record_id,"case_id":case_id,"review_round_ids":[round_id],"mandatory_child_acceptance_ids":children,"evidence_ids":submission.payload["evidence_ids"],"owner_decision_ids":args["owner_decision_ids"],"reason":args["reason"]}),None)?;
    b.head.payload["acceptance_id"] = json!(r.record_id);
    b.state("accepted")?;
    b.plan.result.data = json!({"acceptance_id":r.record_id,"acceptance_level":kind});
    Ok(())
}

/// Save drafts and publish immutable copies with provenance checked before writing.
fn knowledge(b: &mut Builder<'_>, name: &str, args: &Value) -> Result<()> {
    for id in array(args, "basis_refs") {
        let r = b.s.record(id.as_str().unwrap_or(""), None)?;
        require(
            matches!(r.record_kind.as_str(), "evidence" | "decision"),
            "EVIDENCE_MISSING",
            "Knowledge basis must reference evidence or a decision",
        )?;
    }
    if name == "at_knowledge_save" {
        for association in array(args, "associations") {
            in_scope(b.s, b.p, text(association, "work_id")?)?;
        }
        if matches!(args["basis"].as_str(), Some("verified" | "confirmed")) {
            require(
                !array(args, "basis_refs").is_empty(),
                "EVIDENCE_MISSING",
                "Verified knowledge requires provenance",
            )?;
        }
        if let Some(id) = args["note_id"].as_str() {
            let note = b.s.record(id, Some("note"))?.clone();
            let expected = text(args, "expected_content_hash")?;
            b.plan.effects.push(Effect::UpdateDocument {
                id: text(&note.payload, "document_id")?.into(),
                input: json!({"title":args["title"],"content":args["content"]}),
                previous_hash: expected.into(),
            });
            let mut payload = note.payload.clone();
            for field in [
                "title",
                "associations",
                "knowledge_state",
                "basis",
                "basis_refs",
            ] {
                payload[field] = args[field].clone();
            }
            payload["draft_hash"] = json!(content_hash(text(args, "content")?));
            b.update(note, payload)?;
            b.plan.result.data =
                json!({"note_id":id,"content_hash":content_hash(text(args,"content")?)});
        } else {
            let id = Uuid::new_v4().to_string();
            b.plan.effects.push(Effect::CreateDocument{input:json!({"id":id,"title":args["title"],"content":args["content"],"projectId":b.s.config.payload["general_project_id"]})});
            let r=b.add("note",json!({"document_id":id,"title":args["title"],"draft_hash":content_hash(text(args,"content")?),"associations":args["associations"],"knowledge_state":args["knowledge_state"],"basis":args["basis"],"basis_refs":args["basis_refs"],"publication_ids":[]}),None)?;
            b.plan.result.data = json!({"note_id":r.record_id,"document_id":id,"content_hash":r.payload["draft_hash"]});
        }
    } else {
        let note = b.s.record(text(args, "note_id")?, Some("note"))?.clone();
        require(
            b.s.is_committed(&note),
            "KNOWLEDGE_UNPUBLISHED",
            "Draft save has not committed",
        )?;
        b.pins(array(args, "outgoing_publications"))?;
        // Native content is filled from an exact checked read by the gateway before executing this plan.
        let document_id = Uuid::new_v4().to_string();
        b.plan.effects.push(Effect::CopyDocument {
            source_id: text(&note.payload, "document_id")?.into(),
            id: document_id.clone(),
            title: format!("{} · publication", text(&note.payload, "title")?),
            project_id: text(&b.s.config.payload, "general_project_id")?.into(),
            content_hash: text(args, "expected_content_hash")?.into(),
        });
        let r=b.add("publication",json!({"note_id":note.record_id,"document_id":document_id,"content_hash":args["expected_content_hash"],"version":array(&note.payload,"publication_ids").len()+1,"knowledge_state":note.payload["knowledge_state"],"basis":note.payload["basis"],"basis_refs":args["basis_refs"],"associations":note.payload["associations"],"outgoing_publications":args["outgoing_publications"],"reason":args["reason"]}),None)?;
        let mut payload = note.payload.clone();
        let mut ids = array(&payload, "publication_ids").to_vec();
        ids.push(json!(r.record_id));
        payload["publication_ids"] = json!(ids);
        payload["latest_publication_id"] = json!(r.record_id);
        payload["draft_hash"] = args["expected_content_hash"].clone();
        b.update(note, payload)?;
        b.plan.result.data =
            json!({"publication_id":r.record_id,"content_hash":r.payload["content_hash"]});
    }
    Ok(())
}

/// Record owner decisions only from protected identity or explicitly allowed attribution.
fn owner_decide(b: &mut Builder<'_>, args: &Value) -> Result<()> {
    let target = b.s.record(text(args, "record_id")?, None)?.clone();
    let kind = text(args, "source_kind")?;
    match kind {
        "owner_authenticated" => require(
            b.p.role == Role::Owner,
            "UNTRUSTED_OWNER_SOURCE",
            "Owner-authenticated decisions require an owner binding",
        )?,
        "root_attributed" => {
            require(
                b.p.role == Role::Root
                    && b.s.config.payload["policy"]["allow_root_attributed_owner_decisions"]
                        == true,
                "UNTRUSTED_OWNER_SOURCE",
                "Policy does not permit root attribution",
            )?;
            text(&args["source_reference"], "locator")?;
        }
        "linear_comment" => {
            require(
                b.s.config.payload["policy"]["integration_uses_owner_pat"] == false,
                "UNTRUSTED_OWNER_SOURCE",
                "Owner PAT comments cannot prove independent human approval",
            )?;
            b.plan.effects.push(Effect::VerifyOwnerComment {
                id: text(args, "source_comment_id")?.into(),
                work_id: target.work_id.clone(),
                decision_token: text(&target.payload, "decision_token")?.into(),
                decision_key: text(args, "decision_key")?.into(),
                owner_ids: array(&b.s.config.payload, "owner_linear_user_ids")
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
            });
        }
        _ => {
            return Err(Fault::new(
                "UNTRUSTED_OWNER_SOURCE",
                "Unsupported owner decision source",
            ));
        }
    }
    if target.record_kind == "question" {
        require(
            target.payload["status"] == "open"
                && array(&target.payload, "options")
                    .iter()
                    .any(|o| o["key"] == args["decision_key"]),
            "INVALID_INPUT",
            "Question is closed or the option does not exist",
        )?;
    } else {
        require(
            matches!(target.record_kind.as_str(), "change_proposal" | "finding"),
            "INVALID_INPUT",
            "This record cannot receive an owner decision",
        )?;
    }
    let r=b.add("decision",json!({"target_record_id":target.record_id,"decision_key":args["decision_key"],"rationale":args["rationale"],"source_kind":kind,"source_reference":args["source_reference"],"source_comment_id":args["source_comment_id"],"policy_version":b.s.config.payload["policy_version"],"trust_basis":if kind=="owner_authenticated"{"authenticated_owner"}else if kind=="linear_comment"{"separate_app_human_comment"}else{"reported_by_root"}}),None)?;
    if target.record_kind != "finding" {
        let mut payload = target.payload.clone();
        payload["status"] = json!(if target.record_kind == "question" {
            "answered"
        } else if args["decision_key"] == "approve" {
            "approved"
        } else {
            "rejected"
        });
        payload["decision_id"] = json!(r.record_id);
        b.update(target, payload)?;
    }
    b.plan.result.data = json!({"decision_id":r.record_id});
    Ok(())
}

/// Compute an epic candidate's completeness from exact child submissions and acceptances.
fn candidate(b: &mut Builder<'_>, args: &Value) -> Result<()> {
    let plan = b.current_plan(args)?;
    require(
        b.s.work(&b.work)?.identity.payload["kind"] == "epic",
        "INVALID_PARENT",
        "Composition candidates belong to epics",
    )?;
    let mut child_results = vec![];
    let mut included = BTreeSet::new();
    for id in array(args, "included_submissions") {
        let r =
            b.s.record(id.as_str().unwrap_or(""), Some("submission"))?
                .clone();
        require(
            b.s.work(&r.work_id)?.identity.payload["primary_parent_id"] == b.work,
            "OUT_OF_SCOPE",
            "Candidate contains a foreign module",
        )?;
        require(
            included.insert(r.work_id.clone()),
            "INVALID_INPUT",
            "Candidate repeats a module",
        )?;
        b.verify_document(&r);
        if let Some(a) = b.s.work(&r.work_id)?.head.payload["acceptance_id"].as_str() {
            require(
                b.s.record(a, Some("acceptance"))?.payload["submission_id"] == r.record_id,
                "STALE_SUBMISSION",
                "Module acceptance refers to a different submission",
            )?;
            child_results.push(a.to_owned());
        }
    }
    let mandatory: BTreeSet<_> = array(&plan.payload, "mandatory_children")
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    let complete = mandatory == included && child_results.len() == mandatory.len();
    if args["kind"] == "final" {
        require(
            complete,
            "INCOMPLETE_CANDIDATE",
            "Final candidate needs every exact accepted module",
        )?;
    }
    let evidence = b.evidence(array(args, "evidence"))?;
    require(
        array(args, "evidence")
            .iter()
            .any(|e| e["kind"] == "integration" && e["result"] == "passed"),
        "EVIDENCE_MISSING",
        "Composition requires passed integration evidence",
    )?;
    let subject = hash(&args["artifacts"])?;
    let ordered = b.child_results(&plan).unwrap_or(child_results);
    let r=b.snapshot("candidate","Epic composition",json!({"epic_id":b.work,"kind":args["kind"],"included_submissions":args["included_submissions"],"accepted_module_result_ids":ordered,"artifacts":args["artifacts"],"evidence_ids":evidence,"environment":args["environment"],"observation_source":args["observation_source"],"subject_hash":subject,"computed_complete":complete}))?;
    b.head.payload["candidate_id"] = json!(r.record_id);
    b.plan.result.data =
        json!({"candidate_id":r.record_id,"computed_complete":complete,"subject_hash":subject});
    Ok(())
}

/// Revoke the old generation only after a primary stop observation covers its last attempt.
fn transfer(b: &mut Builder<'_>, args: &Value) -> Result<()> {
    b.current_plan(args)?;
    let assignment =
        b.s.record(text(args, "assignment_id")?, Some("assignment"))?
            .clone();
    require(
        assignment.work_id == b.work,
        "OUT_OF_SCOPE",
        "Assignment belongs to another work",
    )?;
    let stopped =
        b.s.record(text(args, "stop_observation_id")?, Some("attempt"))?;
    require(
        stopped.payload["assignment_id"] == assignment.record_id
            && stopped.payload["generation"] == assignment.payload["generation"]
            && stopped.payload["observation"]["writer_state"] == "stopped"
            && assignment.payload["latest_attempt_id"] == stopped.record_id,
        "WRITER_ACTIVE",
        "Transfer requires the latest primary stopped observation",
    )?;
    b.stopped()?;
    let previous = assignment.payload["principal_id"].clone();
    let old_generation = assignment.payload["generation"].as_u64().unwrap_or(1);
    let mut payload = assignment.payload.clone();
    payload["principal_id"] = args["new_principal_id"].clone();
    payload["generation"] = json!(old_generation + 1);
    payload["status"] = json!("recovering");
    b.update(assignment.clone(), payload)?;
    for permit in b.s.records.values().filter(|r| {
        r.record_kind == "writer_permit"
            && r.payload["assignment_id"] == assignment.record_id
            && r.payload["status"] == "active"
    }) {
        let mut payload = permit.payload.clone();
        payload["status"] = json!("revoked");
        payload["stop_observation_id"] = args["stop_observation_id"].clone();
        b.update(permit.clone(), payload)?;
    }
    let r=b.add("transfer",json!({"assignment_id":assignment.record_id,"previous_principal_id":previous,"new_principal_id":args["new_principal_id"],"previous_generation":old_generation,"new_generation":old_generation+1,"stop_observation_id":stopped.record_id,"preserved_artifacts":args["preserved_artifacts"],"reason":args["reason"]}),None)?;
    b.plan.result.data = json!({"transfer_id":r.record_id,"assignment_id":assignment.record_id,"generation":old_generation+1,"status":"recovering"});
    Ok(())
}
