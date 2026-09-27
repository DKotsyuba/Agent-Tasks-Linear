//! Pure workflow guidance derived from the authoritative guard rules.
//!
//! Both projections in this module are read-only computations over an already loaded complete
//! Project graph: they reuse [`crate::rules::transition`] and [`crate::rules::discrepancies`]
//! plus the recorded review and integration state, and never contact Linear or Git. There is
//! no second rule system here — every condition shown is produced by the same guards that
//! enforce mutations. [`guidance`] names the single next workflow action for one work item
//! together with the conditions still blocking it. Stage and action names are advisory
//! vocabulary for readers; they never introduce new Linear statuses or bypass the guards.

use crate::model::{Kind, Meta, Status, Work};
use crate::rules;
use serde_json::{Value, json};

/// Trusted role that may start or reopen work of this kind, mirroring the transition guard
/// that reserves Epic, Module and Atomic starts for the orchestrator. Tasks belong to their
/// executing lead.
fn mover(kind: Kind) -> &'static str {
    if kind == Kind::Task {
        "worker"
    } else {
        "orchestrator"
    }
}

/// Role responsible for preparing and submitting this kind of result. Executing leads report
/// on Task, Module and Atomic work; only Epic coordination stays with the orchestrator.
fn submitter(kind: Kind) -> &'static str {
    if kind == Kind::Epic {
        "orchestrator"
    } else {
        "worker"
    }
}

/// Name of the existing tool that edits this kind's readable fields.
fn edit_tool(kind: Kind) -> &'static str {
    match kind {
        Kind::Epic => "edit_epic",
        Kind::Module => "edit_module",
        Kind::Task => "edit_task",
        Kind::Atomic => "edit_atomic",
    }
}

/// Build one advisory next-action description. `kind` names the action, `actor_role` states
/// the responsible role, `tool` names the existing MCP tool that performs it or null when no
/// single call executes the concept, and `target_status` is the status the named `move_status`
/// call would set, or null for non-transition actions. This is guidance, never a complete
/// mutation argument pack.
fn action(
    kind: &str,
    actor_role: &str,
    tool: Option<&str>,
    target_status: Option<Status>,
) -> Value {
    json!({"kind": kind, "actor_role": actor_role, "tool": tool, "target_status": target_status})
}

/// Whether any condition asks for a missing readable field, using the stable guard prefix.
fn needs_fields(conditions: &[String]) -> bool {
    conditions.iter().any(|c| c.starts_with("Required field:"))
}

/// Whether one discrepancy message is forgiven by the explicit reopen path, mirroring the two
/// native-state mismatches the transition guards retain when moving back to In Progress.
fn cleared_by_reopen(message: &str) -> bool {
    message.starts_with("Native status differs") || message.starts_with("Completion changed")
}

/// Guidance tuple: advisory stage, optional next action, and the guard conditions that still
/// apply (empty when the named action is currently allowed).
type Advised = (&'static str, Option<Value>, Vec<String>);

/// Project the single next workflow action for one work item from a complete Project graph.
///
/// Returns `{work_id, stage, next_action, conditions}`. `stage` is advisory vocabulary —
/// `preparation`, `working`, `review`, `merge`, `closure`, `fixes`, `recovery`, `done` or
/// `excluded` — never a new Linear status. `next_action` follows the shape built by
/// [`action`] or is null when no single next MCP action is determined; `conditions` are the
/// authoritative guard messages still blocking progress. Work with a pending write or native
/// drift is always recovery, never done; a pending write names its exact retry tool. A
/// Module whose review is accepted but whose merge is not yet reported stays in the merge
/// stage with an explicit action — absence of a merge report is never treated as proof of an
/// open or closed pull request. Unmanaged issues report their condition with no action. The
/// function performs no I/O and mutates nothing.
pub fn guidance(w: &Work, graph: &[Work]) -> Value {
    let Some(m) = &w.meta else {
        return json!({
            "work_id": w.id(),
            "stage": "preparation",
            "next_action": null,
            "conditions": rules::discrepancies(w, graph),
        });
    };
    let status = w.status().unwrap_or(m.status);
    let drift = rules::discrepancies(w, graph);
    let (stage, next, conditions) = if let Some(pending) = &m.pending {
        let request = &pending.request;
        let role = request["arguments"]["actor_role"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| {
                if request["tool"] == "record_review" {
                    "reviewer".to_owned()
                } else {
                    mover(m.kind).to_owned()
                }
            });
        (
            "recovery",
            Some(action(
                "retry_operation",
                &role,
                request["tool"].as_str(),
                None,
            )),
            drift,
        )
    } else if !drift.is_empty() {
        // Only the two reopen-forgiven mismatches recover through an explicit reopen; any
        // structural drift keeps recovery without naming a single resolving call.
        if drift.iter().all(|d| cleared_by_reopen(d)) {
            (
                "recovery",
                Some(action(
                    "start_work",
                    mover(m.kind),
                    Some("move_status"),
                    Some(Status::InProgress),
                )),
                drift,
            )
        } else {
            ("recovery", None, drift)
        }
    } else {
        match status {
            Status::Backlog => (
                "preparation",
                Some(action(
                    "prepare_work",
                    mover(m.kind),
                    Some("move_status"),
                    Some(Status::Todo),
                )),
                vec![],
            ),
            Status::Todo => {
                let blockers = rules::transition(w, graph, Status::InProgress, mover(m.kind));
                let next = if blockers.is_empty() {
                    Some(action(
                        "start_work",
                        mover(m.kind),
                        Some("move_status"),
                        Some(Status::InProgress),
                    ))
                } else if needs_fields(&blockers) {
                    Some(action(
                        "prepare_work",
                        mover(m.kind),
                        Some(edit_tool(m.kind)),
                        None,
                    ))
                } else {
                    None
                };
                ("preparation", next, blockers)
            }
            Status::InProgress => working(w, m, graph),
            Status::InReview => reviewing(w, m, graph),
            Status::Done => finished(w, graph),
            Status::Canceled | Status::Duplicate => ("excluded", None, vec![]),
        }
    };
    json!({"work_id": w.id(), "stage": stage, "next_action": next, "conditions": conditions})
}

/// Guidance while work is In Progress: restart a stale integration run, finish results and
/// commits, or submit for review (Tasks close directly, having no separate review).
/// Blockers that ask for this work's own fields name the preparing action; structural
/// blockers such as unfinished children leave the action null and the conditions visible.
fn working(w: &Work, m: &Meta, graph: &[Work]) -> Advised {
    if w.fields["work_type"] == "integration" && rules::restart_integration(w, graph) {
        return restart(w, graph);
    }
    let target = if m.kind == Kind::Task {
        Status::Done
    } else {
        Status::InReview
    };
    let role = submitter(m.kind);
    let blockers = rules::transition(w, graph, target, role);
    if blockers.is_empty() {
        let kind = if m.kind == Kind::Task {
            "close_work"
        } else {
            "request_review"
        };
        return (
            "working",
            Some(action(kind, role, Some("move_status"), Some(target))),
            blockers,
        );
    }
    let next = if m.kind == Kind::Epic {
        if blockers.iter().any(|c| c.contains("integration coverage")) {
            // Delivering the missing integration seam has no single executable call.
            Some(action("run_integration", role, None, None))
        } else if needs_fields(&blockers) {
            Some(action(
                "prepare_result",
                role,
                Some(edit_tool(m.kind)),
                None,
            ))
        } else {
            None
        }
    } else if blockers.iter().any(|c| c == "Required field: commit_url") {
        // Code work without an imported current-round snapshot imports its commits first;
        // the import also carries the author's result and check sections.
        Some(action("attach_commits", role, Some("record_commits"), None))
    } else if needs_fields(&blockers) {
        Some(action(
            "prepare_result",
            role,
            Some(edit_tool(m.kind)),
            None,
        ))
    } else {
        None
    };
    ("working", next, blockers)
}

/// Guidance while non-Task work sits In Review, following the recorded review state and the
/// closure guards: record a fresh verdict, apply requested fixes, report the merge, or close.
/// An accepted review never closes a Module by itself; the unreported merge keeps its action.
fn reviewing(w: &Work, m: &Meta, graph: &[Work]) -> Advised {
    let blockers = rules::transition(w, graph, Status::Done, "orchestrator");
    if m.kind == Kind::Task {
        // Tasks never enter review through the guards; no Task review action exists.
        return ("review", None, blockers);
    }
    if w.fields["work_type"] == "integration" && !rules::integration_matches(w, graph) {
        return restart(w, graph);
    }
    if blockers.is_empty() {
        return (
            "closure",
            Some(action(
                "close_work",
                "orchestrator",
                Some("move_status"),
                Some(Status::Done),
            )),
            blockers,
        );
    }
    let current = m
        .review
        .as_ref()
        .is_some_and(|r| r.accepted && r.round == m.round && r.revision == m.revision);
    let (stage, next) = match m.review.as_ref().map(|r| r.accepted) {
        None => (
            "review",
            Some(action(
                "record_review",
                "reviewer",
                Some("record_review"),
                None,
            )),
        ),
        Some(false) => ("fixes", Some(action("apply_fixes", "worker", None, None))),
        Some(true) if !current => (
            "review",
            Some(action(
                "record_review",
                "reviewer",
                Some("record_review"),
                None,
            )),
        ),
        Some(true) if m.kind == Kind::Module && !rules::filled(&w.fields, "merge_report") => (
            "merge",
            Some(action(
                "record_merge",
                "orchestrator",
                Some("edit_module"),
                None,
            )),
        ),
        Some(true) => ("closure", None),
    };
    (stage, next, blockers)
}

/// Guidance for work already Done: only an integration whose recorded module snapshot no
/// longer matches needs attention; everything else is finished with no further action.
fn finished(w: &Work, graph: &[Work]) -> Advised {
    if w.fields["work_type"] == "integration" && !rules::integration_matches(w, graph) {
        return restart(w, graph);
    }
    ("done", None, vec![])
}

/// Guidance for integration work that must run again: the explicit restart move, with the
/// same start conditions the restart itself must meet. A completed integration recovering
/// from staleness reports the recovery stage; an active restart is still working.
fn restart(w: &Work, graph: &[Work]) -> Advised {
    let blockers = rules::transition(w, graph, Status::InProgress, "orchestrator");
    let stage = if w.status().ok() == Some(Status::Done) {
        "recovery"
    } else {
        "working"
    };
    (
        stage,
        Some(action(
            "run_integration",
            "orchestrator",
            Some("move_status"),
            Some(Status::InProgress),
        )),
        blockers,
    )
}
