//! Pure mixed-graph Module report checks, using persisted snapshots rather than local Git.
use agent_tasks_linear::{
    git::GitCommit,
    model::{Kind, LocalGitReport, Meta, Status, Work},
    reports::module_report,
};
use serde_json::{Value, json};

/// Build one managed native-shaped work item with round one, supplied fields and source history.
fn work(
    id: &str,
    kind: Kind,
    status: Status,
    parent: Option<&str>,
    fields: Value,
    reports: Vec<LocalGitReport>,
) -> Work {
    let meta: Meta = serde_json::from_value(json!({"schema":2,"kind":kind,"fields":fields,"project_id":"project","parent_id":parent,"status":status,"round":1,"revision":1,"description":"","creation":{},"git_reports":reports})).unwrap();
    Work {
        native: json!({"id":id,"identifier":format!("ISSUE-{id}"),"title":id,"url":format!("https://linear.app/issue/{id}"),"state":{"name":status.name()},"parent":parent.map(|id| json!({"id":id}))}),
        fields,
        meta: Some(meta),
    }
}

/// One immutable imported snapshot; its round controls whether the composer may use it.
fn source(round: u64, result: &str) -> LocalGitReport {
    LocalGitReport {
        round,
        commit: GitCommit {
            repository_identity: "/repo/.git".into(),
            repository_path: "/repo".into(),
            sha: "a".repeat(40),
            subject: "feat: shared change".into(),
            original_message:
                "feat: shared change\n\nResult:\nShared result\n\nChecks:\nShared check\n".into(),
            result: result.into(),
            checks: "Shared check".into(),
            notes: Some("Known limitation".into()),
            author: "Fixture <fixture@example.test>".into(),
            authored_at: "2026-01-01T00:00:00Z".into(),
        },
    }
}

/// Native progress excludes retired work; shared sources render once and prior rounds stay hidden.
#[test]
fn mixed_module_preserves_manual_work_and_current_sources() {
    let module = work(
        "module",
        Kind::Module,
        Status::InProgress,
        None,
        json!({"result":"Stale module result","check_result":"Stale module checks"}),
        vec![],
    );
    let graph = vec![
        work(
            "code-one",
            Kind::Task,
            Status::Done,
            Some("module"),
            json!({"work_type":"code"}),
            vec![source(1, "Shared result")],
        ),
        work(
            "code-two",
            Kind::Task,
            Status::Done,
            Some("module"),
            json!({"work_type":"code"}),
            vec![source(1, "Shared result")],
        ),
        work(
            "document",
            Kind::Task,
            Status::Done,
            Some("module"),
            json!({"work_type":"non_code","result":"Document result","check_result":"Document check","artifact_url":"https://example.test/document"}),
            vec![],
        ),
        work(
            "reopened",
            Kind::Task,
            Status::InProgress,
            Some("module"),
            json!({}),
            vec![source(0, "Old round result")],
        ),
        work(
            "canceled",
            Kind::Task,
            Status::Canceled,
            Some("module"),
            json!({"reason":"No longer needed"}),
            vec![],
        ),
        work(
            "duplicate",
            Kind::Atomic,
            Status::Duplicate,
            Some("module"),
            json!({"reason":"Already covered"}),
            vec![],
        ),
    ];
    let report = module_report(&module, &graph).unwrap();
    assert_eq!(
        (report.tasks_done, report.tasks_total, report.excluded_count),
        (3, 4, 2)
    );
    assert_eq!(report.unfinished.len(), 1);
    assert_eq!(report.unfinished[0].work_id, "reopened");
    assert_eq!(report.excluded.len(), 2);
    assert_eq!(report.source_commits.len(), 1);
    assert_eq!(report.source_commits[0].work_ids.len(), 2);
    assert_eq!(report.summary.matches("Shared result").count(), 1);
    assert_eq!(report.reported_checks.matches("Shared check").count(), 1);
    assert!(report.summary.contains("Document result"));
    assert!(report.summary.contains("https://example.test/document"));
    assert!(!report.summary.contains("Old round result"));
    assert!(!report.summary.contains("Stale module result"));
    assert!(report.notes.contains("Known limitation"));
    assert!(report.notes.contains("No longer needed"));
    assert!(report.pr_draft.contains("## Unfinished"));
    assert!(report.pr_draft.contains("## Excluded"));
    let mut reversed = graph.clone();
    reversed.reverse();
    assert_eq!(
        serde_json::to_value(module_report(&module, &reversed).unwrap()).unwrap(),
        serde_json::to_value(report).unwrap()
    );
}

/// Legacy empty Modules retain manual results; missing metadata and oversized results are explicit.
#[test]
fn incomplete_or_oversized_reports_never_masquerade_as_empty_success() {
    let module = work(
        "module",
        Kind::Module,
        Status::InProgress,
        None,
        json!({"result":"Manual result","check_result":"Manual check"}),
        vec![],
    );
    let report = module_report(&module, &[]).unwrap();
    assert_eq!(report.summary, "Manual result");
    assert_eq!(report.reported_checks, "Manual check");
    let mut unknown = work(
        "child",
        Kind::Task,
        Status::Done,
        Some("module"),
        json!({}),
        vec![],
    );
    unknown.meta = None;
    assert_eq!(
        module_report(&module, &[unknown]).unwrap_err().code,
        "UNMANAGED_ITEM"
    );
    let big = work(
        "child",
        Kind::Task,
        Status::Done,
        Some("module"),
        json!({"result":"a".repeat(30_000)}),
        vec![],
    );
    assert_eq!(
        module_report(&module, &[big]).unwrap_err().code,
        "REPORT_LIMIT"
    );
}
