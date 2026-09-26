//! Workflow guards: top-down starts, local checkout validation, bottom-up closure and frozen epics.
use crate::model::{Fault, Kind, Result, Status, Work};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// All standard target statuses returned by context, including blocked transitions.
pub const STATUSES: [Status; 7] = [
    Status::Backlog,
    Status::Todo,
    Status::InProgress,
    Status::InReview,
    Status::Done,
    Status::Canceled,
    Status::Duplicate,
];
/// Whether a readable string field is present and nonblank.
pub fn filled(fields: &Value, key: &str) -> bool {
    fields[key].as_str().is_some_and(|s| !s.trim().is_empty())
}
/// Collect IDs from the optional integration module list.
pub fn module_ids(fields: &Value) -> Vec<String> {
    fields["integration_modules"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}
/// Find a work item in a complete project graph.
pub fn find<'a>(graph: &'a [Work], id: &str) -> Option<&'a Work> {
    graph.iter().find(|w| w.id() == id)
}
/// Native parent ID, with no issue parent for direct project children.
pub fn parent(w: &Work) -> Option<&str> {
    w.native["parent"]["id"].as_str()
}
/// Native direct children, including archived work so retirement cannot hide unfinished scope.
pub fn children<'a>(graph: &'a [Work], id: &str) -> Vec<&'a Work> {
    graph.iter().filter(|w| parent(w) == Some(id)).collect()
}
/// Native or recorded children: a manual detach must not remove work from its original scope.
fn expected_children<'a>(graph: &'a [Work], w: &Work) -> Vec<&'a Work> {
    graph
        .iter()
        .filter(|child| {
            parent(child) == Some(w.id())
                || child
                    .meta
                    .as_ref()
                    .is_some_and(|m| m.parent_id.as_deref() == Some(w.id()))
                || w.meta
                    .as_ref()
                    .is_some_and(|m| m.children.iter().any(|id| id == child.id()))
        })
        .collect()
}
/// Completion identity used to invalidate integration after a module reopens or changes.
pub fn completion(w: &Work) -> String {
    format!(
        "{}:{}:{}",
        w.meta.as_ref().map(|m| m.round).unwrap_or(0),
        w.meta.as_ref().map(|m| m.revision).unwrap_or(0),
        w.native["completedAt"].as_str().unwrap_or("")
    )
}
/// True only for a successfully reviewed integration run matching all participating module completions.
pub fn integration_current(w: &Work, graph: &[Work]) -> bool {
    w.status().ok() == Some(Status::Done) && integration_matches(w, graph)
}
/// Require all recorded integration inputs to remain merged, unchanged and free of native drift.
fn integration_matches(w: &Work, graph: &[Work]) -> bool {
    let Some(m) = &w.meta else { return false };
    let ids = module_ids(&w.fields);
    w.fields["work_type"] == "integration"
        && ids.len() >= 2
        && ids.iter().all(|id| {
            find(graph, id).is_some_and(|v| {
                v.status().ok() == Some(Status::Done)
                    && v.meta.as_ref().is_some_and(|m| m.kind == Kind::Module)
                    && filled(&v.fields, "merge_report")
                    && m.integration.get(id) == Some(&completion(v))
                    && discrepancies(v, graph).is_empty()
            })
        })
}
/// Whether an active integration needs a fresh run after any participating Module changes.
/// This permits an explicit In Progress-to-In Progress restart without losing earlier history.
pub fn restart_integration(w: &Work, graph: &[Work]) -> bool {
    w.fields["work_type"] == "integration"
        && w.status().ok() == Some(Status::InProgress)
        && w.meta.as_ref().is_some_and(|m| m.round > 0)
        && !integration_matches(w, graph)
}
/// Validate one native parent against the allowed hierarchy; a frozen epic cannot acquire a module.
pub fn hierarchy(
    kind: Kind,
    parent_id: Option<&str>,
    graph: &[Work],
    existing: Option<&str>,
) -> Vec<String> {
    let mut errors = vec![];
    match parent_id {
        None => {
            if kind == Kind::Task {
                errors.push("Task requires a Module parent".into())
            }
        }
        Some(id) => match find(graph, id).and_then(|w| w.meta.as_ref()) {
            None => errors.push("Parent must be a managed issue in the same project".into()),
            Some(p) => {
                let valid = matches!(
                    (kind, p.kind),
                    (Kind::Module, Kind::Epic)
                        | (Kind::Task, Kind::Module)
                        | (Kind::Atomic, Kind::Epic | Kind::Module)
                );
                if !valid {
                    errors.push("Invalid parent for this work type".into());
                }
                if kind == Kind::Module
                    && p.frozen_modules
                        .as_ref()
                        .is_some_and(|ids| existing.is_none_or(|id| !ids.iter().any(|x| x == id)))
                {
                    errors.push(
                        "Epic composition is frozen; create this Module under Project in Todo"
                            .into(),
                    );
                }
            }
        },
    }
    errors
}
/// Report native drift and structural violations without changing Linear.
pub fn discrepancies(w: &Work, graph: &[Work]) -> Vec<String> {
    let Some(m) = &w.meta else {
        return vec!["Issue has no MCP workflow data".into()];
    };
    let mut e = vec![];
    if m.pending.is_some() {
        e.push("A write is pending; retry the same request_id and arguments".into());
    }
    if w.status().ok() != Some(m.status) {
        e.push("Native status differs from the recorded transition; restore it in Linear or reopen through move_status".into());
    }
    if w.native["project"]["id"] != m.project_id {
        e.push("Native project was changed manually".into());
    }
    if parent(w) != m.parent_id.as_deref() {
        e.push("Native parent was changed manually".into());
    }
    for id in &m.children {
        if !find(graph, id).is_some_and(|child| {
            parent(child) == Some(w.id()) && child.native["project"]["id"] == m.project_id
        }) {
            e.push(format!(
                "Recorded child was detached, moved or deleted: {id}"
            ));
        }
    }
    for child in expected_children(graph, w) {
        if parent(child) != Some(w.id()) || child.native["project"]["id"] != m.project_id {
            e.push(format!("Child membership changed manually: {}", child.id()));
        }
    }
    if w.native["description"].as_str().unwrap_or("") != m.description {
        e.push(
            "Description changed in Linear; use edit to adopt and validate the current fields"
                .into(),
        );
    }
    if !w.native["labels"]["nodes"]
        .as_array()
        .is_some_and(|a| a.iter().any(|x| x["name"] == m.kind.label()))
    {
        e.push("Native type label is missing".into());
    }
    if w.native["labels"]["pageInfo"]["hasNextPage"] == true {
        e.push("Issue labels are truncated".into());
    }
    if w.status().ok() == Some(Status::Done)
        && w.native["completedAt"].as_str() != m.completed_at.as_deref()
    {
        e.push("Completion changed outside MCP; reopen and record a fresh result/review".into());
    }
    e.extend(hierarchy(m.kind, parent(w), graph, Some(w.id())));
    let mut cursor = Some(w.id());
    let mut seen = BTreeSet::new();
    while let Some(id) = cursor {
        if !seen.insert(id) {
            e.push("Parent cycle detected".into());
            break;
        }
        let Some(item) = find(graph, id) else {
            e.push("Ancestor is outside the project graph".into());
            break;
        };
        if let Some(meta) = &item.meta
            && let Some(expected) = &meta.frozen_modules
        {
            let actual: BTreeSet<_> = children(graph, id)
                .iter()
                .filter(|c| {
                    c.meta.as_ref().is_some_and(|m| m.kind == Kind::Module)
                        || c.native["labels"]["nodes"]
                            .as_array()
                            .is_some_and(|a| a.iter().any(|x| x["name"] == "MODULE"))
                })
                .map(|c| c.id().to_string())
                .collect();
            if actual != expected.iter().cloned().collect() {
                e.push(format!(
                    "Frozen Module membership differs for {}",
                    item.native["identifier"].as_str().unwrap_or(id)
                ));
            }
        }
        cursor = parent(item);
    }
    e
}
/// Append missing required fields to a transition's readable explanations.
fn needs(e: &mut Vec<String>, f: &Value, keys: &[&str]) {
    for key in keys {
        if !filled(f, key) {
            e.push(format!("Required field: {key}"));
        }
    }
}
/// Reject incomplete children, rather than closing them automatically.
fn completed_children(e: &mut Vec<String>, w: &Work, graph: &[Work]) {
    for c in expected_children(graph, w) {
        if !c.status().is_ok_and(Status::terminal) {
            e.push(format!(
                "Child is unfinished: {}",
                c.native["identifier"].as_str().unwrap_or(c.id())
            ));
        }
        if c.meta.is_some() {
            e.extend(discrepancies(c, graph));
            if c.fields["work_type"] == "integration"
                && c.status().ok() == Some(Status::Done)
                && !integration_current(c, graph)
            {
                e.push("Completed integration is stale; reopen and repeat it".into());
            }
        }
    }
}
/// Require repository identity for a coding checkout, retaining legacy URL-only readiness.
/// A local path triggers bounded read-only Git checks of both it and any supplied worktree;
/// failures become guard conditions. Missing worktree/branch fields are handled by the caller.
fn checkout(e: &mut Vec<String>, fields: &Value) {
    if let Some(path) = fields["repository_path"].as_str() {
        for path in std::iter::once(path).chain(fields["worktree"].as_str()) {
            if let Err(error) = crate::git::validate_repository(path) {
                e.push(error.to_string());
            }
        }
    } else if !filled(fields, "repository_url") {
        e.push("Required field: repository_path or repository_url".into());
    }
}

/// Check start prerequisites against a fresh full graph, probing configured local Git checkouts.
fn start(e: &mut Vec<String>, w: &Work, graph: &[Work]) {
    let m = w.meta.as_ref().unwrap();
    let f = &w.fields;
    needs(e, f, &["expected_result", "acceptance_criteria"]);
    if let Some(id) = parent(w) {
        if !find(graph, id).is_some_and(|p| p.status().ok() == Some(Status::InProgress)) {
            e.push("Start the parent in In Progress first".into());
        }
        if let Some(p) = find(graph, id) {
            e.extend(discrepancies(p, graph));
        }
    }
    match m.kind {
        Kind::Epic => {
            needs(e, f, &["business_requirements"]);
            if children(graph, w.id()).iter().any(|w| w.meta.is_none()) {
                e.push(
                    "All Epic children must have workflow data before its composition is frozen"
                        .into(),
                );
            }
        }
        Kind::Module => {
            needs(
                e,
                f,
                &[
                    "lead",
                    "branch",
                    "worktree",
                    "required_contract",
                    "provided_contract",
                ],
            );
            checkout(e, f);
            if let Some(id) = f["after_epic"].as_str() {
                if parent(w).is_some() {
                    e.push("after_epic is only valid for a Project-level Module".into());
                }
                if !find(graph, id).is_some_and(|p| {
                    p.meta.as_ref().is_some_and(|m| m.kind == Kind::Epic)
                        && p.status().ok() == Some(Status::Done)
                        && discrepancies(p, graph).is_empty()
                }) {
                    e.push("The waiting Epic must be Done in this project".into());
                }
            }
        }
        Kind::Task => needs(e, f, &["local_check"]),
        Kind::Atomic => {
            needs(e, f, &["executor", "local_check"]);
            if f["work_type"] == "code" {
                let inherited = parent(w)
                    .and_then(|id| find(graph, id))
                    .filter(|p| p.meta.as_ref().is_some_and(|m| m.kind == Kind::Module));
                if inherited.is_none() {
                    needs(e, f, &["branch", "worktree"]);
                    checkout(e, f);
                }
            }
            if f["work_type"] == "integration" {
                needs(e, f, &["scenarios", "environment"]);
                if parent(w)
                    .and_then(|id| find(graph, id))
                    .is_some_and(|p| p.meta.as_ref().is_some_and(|m| m.kind != Kind::Epic))
                {
                    e.push("Integration belongs under Project or Epic".into());
                }
                let ids = module_ids(f);
                if ids.len() < 2 {
                    e.push("Integration needs at least two participating Modules".into());
                }
                for id in ids {
                    match find(graph, &id) {
                        Some(p)
                            if p.meta.as_ref().is_some_and(|m| m.kind == Kind::Module)
                                && p.status().ok() == Some(Status::Done)
                                && filled(&p.fields, "merge_report") =>
                        {
                            e.extend(discrepancies(p, graph));
                            if parent(w).is_some() && parent(p) != parent(w) {
                                e.push("Epic integration must use Modules from that Epic".into());
                            }
                        }
                        _ => e.push(format!("Integration Module is not merged and Done: {id}")),
                    }
                }
            }
        }
    }
}
/// Check a result before submission/closure. Non-code work requires an artifact URL, not a commit.
fn result(e: &mut Vec<String>, w: &Work) {
    needs(e, &w.fields, &["result", "check_result"]);
    if w.fields["work_type"] == "code" {
        needs(e, &w.fields, &["commit_url"]);
    } else {
        needs(e, &w.fields, &["artifact_url"]);
    }
}
/// Evaluate an explicit transition; context uses this same function for check-only output.
/// `role` is trusted caller attribution, not an authentication permission system.
pub fn transition(w: &Work, graph: &[Work], target: Status, role: &str) -> Vec<String> {
    let Some(m) = &w.meta else {
        return vec!["Unmanaged work cannot be moved".into()];
    };
    let mut e = discrepancies(w, graph);
    let current = w.status().unwrap_or(m.status);
    // Reopening is the explicit recovery path for external state/content changes; structural drift still blocks.
    if target == Status::InProgress {
        e.retain(|s| {
            !s.starts_with("Native status differs") && !s.starts_with("Completion changed")
        });
    }
    if current == target && current == m.status && !restart_integration(w, graph) {
        return e;
    }
    if target == Status::InProgress && m.kind != Kind::Task && role != "orchestrator" {
        e.push("Only the orchestrator starts Epic, Module or Atomic work".into());
    }
    if matches!(target, Status::InReview | Status::Done)
        && let Some(p) = parent(w).and_then(|id| find(graph, id))
    {
        if p.status().ok() != Some(Status::InProgress) {
            e.push("Keep the parent In Progress until its children finish".into());
        }
        e.extend(discrepancies(p, graph));
    }
    if target == Status::Canceled || target == Status::Duplicate {
        needs(&mut e, &w.fields, &["reason"]);
        if target == Status::Duplicate {
            needs(&mut e, &w.fields, &["duplicate_of"]);
        }
        if m.kind == Kind::Epic || m.kind == Kind::Module {
            completed_children(&mut e, w, graph);
        }
        return e;
    }
    let allowed = match (current, target) {
        (Status::Backlog, Status::Todo) | (Status::Todo, Status::Backlog) => true,
        (_, Status::InProgress) => true,
        (Status::InProgress, Status::Done) if m.kind == Kind::Task => true,
        (Status::InProgress, Status::InReview) if m.kind != Kind::Task => true,
        (Status::InReview, Status::Done) if m.kind != Kind::Task => true,
        _ => false,
    };
    if !allowed {
        e.push("This transition is not part of this work type's cycle".into());
        return e;
    }
    match target {
        Status::InProgress => start(&mut e, w, graph),
        Status::InReview => match m.kind {
            Kind::Epic => {
                completed_children(&mut e, w, graph);
                needs(&mut e, &w.fields, &["result"]);
                let mods: Vec<_> = children(graph, w.id())
                    .into_iter()
                    .filter(|c| {
                        c.meta.as_ref().is_some_and(|m| m.kind == Kind::Module)
                            && !matches!(
                                c.status().ok(),
                                Some(Status::Canceled | Status::Duplicate)
                            )
                    })
                    .collect();
                if mods.len() > 1 {
                    let covered: BTreeSet<_> = children(graph, w.id())
                        .into_iter()
                        .filter(|a| integration_current(a, graph))
                        .flat_map(|a| module_ids(&a.fields))
                        .collect();
                    if mods.iter().any(|m| !covered.contains(m.id())) {
                        e.push("Every delivered Module requires current successful integration coverage".into());
                    }
                }
            }
            Kind::Module => {
                completed_children(&mut e, w, graph);
                needs(&mut e, &w.fields, &["pr_url", "result", "check_result"]);
            }
            Kind::Atomic => {
                result(&mut e, w);
                if w.fields["work_type"] == "integration" && !integration_matches(w, graph) {
                    e.push("Integration is stale or a participating Module has drift; resolve it and restart the integration".into());
                }
            }
            Kind::Task => e.push("Tasks do not have separate reviews".into()),
        },
        Status::Done => {
            if m.kind == Kind::Task {
                result(&mut e, w);
            } else {
                if role != "orchestrator" {
                    e.push("Only the orchestrator closes reviewed work".into());
                }
                if !m
                    .review
                    .as_ref()
                    .is_some_and(|r| r.accepted && r.round == m.round && r.revision == m.revision)
                {
                    e.push("A positive review of the current result is required".into());
                }
                if m.kind == Kind::Module {
                    needs(&mut e, &w.fields, &["merge_report"]);
                    completed_children(&mut e, w, graph);
                }
                if m.kind == Kind::Epic {
                    completed_children(&mut e, w, graph);
                }
                if m.kind == Kind::Atomic
                    && w.fields["work_type"] == "integration"
                    && !integration_matches(w, graph)
                {
                    e.push("Integration completion snapshot is stale or a participating Module has drift".into());
                }
            }
        }
        _ => {}
    }
    e.sort();
    e.dedup();
    e
}
/// Turn validation explanations into a stable mutation rejection.
pub fn enforce(errors: Vec<String>) -> Result<()> {
    if errors.is_empty() {
        Ok(())
    } else {
        Err(Fault::new("WORKFLOW_BLOCKED", errors.join("; ")))
    }
}
/// Explain all transitions using the exact same guards as move_status.
pub fn actions(w: &Work, graph: &[Work]) -> Value {
    json!(
        STATUSES
            .iter()
            .map(|s| {
                let errors = transition(w, graph, *s, "orchestrator");
                json!({"status":s,"allowed":errors.is_empty(),"conditions":errors})
            })
            .collect::<Vec<_>>()
    )
}
