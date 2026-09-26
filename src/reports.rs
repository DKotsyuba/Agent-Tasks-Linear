//! Pure Module summaries derived from native statuses and persisted current-round child results.
use crate::{
    model::{Kind, Result, Status, Work, require},
    rules,
};
use serde::{Deserialize, Serialize};

/// Native child identity and status for visible unfinished/excluded work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportWork {
    /// Native issue UUID.
    pub work_id: String,
    /// Human issue identifier, falling back to UUID if absent.
    pub identifier: String,
    /// Current native title.
    pub title: String,
    /// Native issue permalink, empty only when absent in source data.
    pub url: String,
    /// Current native status, never inferred from reports or assignments.
    pub status: Status,
}

/// Unique source commit used by one or more children in the current Module summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceCommit {
    /// Canonical common Git directory recorded at import time.
    pub repository_identity: String,
    /// Full immutable commit object ID.
    pub sha: String,
    /// Original Conventional Commit subject.
    pub subject: String,
    /// Native child UUIDs using this commit, each listed once in summary order.
    pub work_ids: Vec<String>,
}

/// Derived current Module result, shared by context, review readiness and PR draft generation.
/// Counts use native child statuses; canceled/duplicate work is excluded from Task totals.
/// Source checks are reported claims. Composition performs no I/O and stores no second result set.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModuleReport {
    /// Readable results grouped by child, with shared commit content rendered only once.
    pub summary: String,
    /// Reported checks grouped by child, with shared commit checks rendered only once.
    pub reported_checks: String,
    /// Current source notes, limitations and exclusion reasons when provided.
    pub notes: String,
    /// Done Task children, excluding Atomics and retired work.
    pub tasks_done: usize,
    /// All non-retired Task children, including unfinished work.
    pub tasks_total: usize,
    /// Number of Canceled/Duplicate direct children, including Atomics.
    pub excluded_count: usize,
    /// Deduplicated commit references across current child rounds.
    pub source_commits: Vec<SourceCommit>,
    /// Nonterminal direct children, including Atomics awaiting review.
    pub unfinished: Vec<ReportWork>,
    /// Retired direct children kept visible outside progress totals.
    pub excluded: Vec<ReportWork>,
    /// Ready-to-copy Markdown PR description; this is never published by composition.
    pub pr_draft: String,
}

/// Compose one managed Module from a complete Store graph, without reading Git or writing state.
/// Uses only each child's current round; manual/non-code result fields remain supported.
/// An empty or fully excluded Module may use its existing manual result/check fields.
/// Unknown child metadata/status or summaries exceeding native 30,000-character field limits fail
/// explicitly. Stable child-ID ordering makes repeated reads deterministic.
pub fn module_report(module: &Work, graph: &[Work]) -> Result<ModuleReport> {
    require(
        module.managed()?.kind == Kind::Module,
        "WRONG_KIND",
        "ModuleReport requires a Module",
    )?;
    let mut report = ModuleReport::default();
    let mut children = rules::children(graph, module.id());
    children.sort_by_key(|child| child.id());
    for child in children {
        let meta = child.managed()?;
        let status = child.status()?;
        let identity = ReportWork {
            work_id: child.id().into(),
            identifier: child.native["identifier"]
                .as_str()
                .unwrap_or(child.id())
                .into(),
            title: child.native["title"].as_str().unwrap_or("").into(),
            url: child.native["url"].as_str().unwrap_or("").into(),
            status,
        };
        let label = format!(
            "{} — {} ({})",
            identity.identifier,
            identity.title,
            status.name()
        );
        if matches!(status, Status::Canceled | Status::Duplicate) {
            report.excluded_count += 1;
            if let Some(reason) = child.fields["reason"].as_str() {
                report
                    .notes
                    .push_str(&format!("### {label}\n\n{reason}\n\n"));
            }
            report.excluded.push(identity);
            continue;
        }
        if meta.kind == Kind::Task {
            report.tasks_total += 1;
            report.tasks_done += usize::from(status == Status::Done);
        }
        if !status.terminal() {
            report.unfinished.push(identity);
        }
        let sources: Vec<_> = meta.current_git_reports().collect();
        report.summary.push_str(&format!("### {label}\n\n"));
        if sources.is_empty() {
            report.summary.push_str(
                child.fields["result"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or("No current-round result."),
            );
            report.summary.push_str("\n\n");
            if let Some(checks) = child.fields["check_result"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
            {
                report
                    .reported_checks
                    .push_str(&format!("### {label}\n\n{checks}\n\n"));
            }
            for key in ["commit_url", "artifact_url"] {
                if let Some(url) = child.fields[key].as_str() {
                    report.summary.push_str(&format!("Source: {url}\n\n"));
                }
            }
        } else {
            for source in sources {
                let commit = &source.commit;
                if let Some(existing) = report.source_commits.iter_mut().find(|s| {
                    s.repository_identity == commit.repository_identity && s.sha == commit.sha
                }) {
                    if !existing.work_ids.iter().any(|id| id == child.id()) {
                        existing.work_ids.push(child.id().into());
                    }
                    report.summary.push_str(&format!(
                        "Shared source already reported: {}.\n\n",
                        commit.sha
                    ));
                    continue;
                }
                report.source_commits.push(SourceCommit {
                    repository_identity: commit.repository_identity.clone(),
                    sha: commit.sha.clone(),
                    subject: commit.subject.clone(),
                    work_ids: vec![child.id().into()],
                });
                report.summary.push_str(&format!("{}\n\n", commit.result));
                report
                    .reported_checks
                    .push_str(&format!("### {label}\n\n{}\n\n", commit.checks));
                if let Some(notes) = &commit.notes {
                    report
                        .notes
                        .push_str(&format!("### {label}\n\n{notes}\n\n"));
                }
            }
        }
    }
    if report.summary.is_empty() {
        report.summary = module.fields["result"].as_str().unwrap_or("").into();
        report.reported_checks = module.fields["check_result"].as_str().unwrap_or("").into();
    }
    for text in [
        &mut report.summary,
        &mut report.reported_checks,
        &mut report.notes,
    ] {
        *text = text.trim().replace("\n## ", "\n### ");
    }
    require(
        report.summary.chars().count() <= 30_000
            && report.reported_checks.chars().count() <= 30_000,
        "REPORT_LIMIT",
        "Module result/checks exceed the native field limit",
    )?;
    report.pr_draft = format!(
        "## Summary\n\n{}\n\n## Reported checks\n\n{}\n\n## Progress\n\n{}/{} Tasks Done; {} excluded.\n",
        report.summary,
        report.reported_checks,
        report.tasks_done,
        report.tasks_total,
        report.excluded_count
    );
    if !report.notes.is_empty() {
        report
            .pr_draft
            .push_str(&format!("\n## Notes\n\n{}\n", report.notes));
    }
    for (title, works) in [
        ("Unfinished", &report.unfinished),
        ("Excluded", &report.excluded),
    ] {
        if !works.is_empty() {
            report.pr_draft.push_str(&format!("\n## {title}\n\n"));
            for work in works {
                report.pr_draft.push_str(&format!(
                    "- [{} — {}]({}): {}\n",
                    work.identifier,
                    work.title,
                    work.url,
                    work.status.name()
                ));
            }
        }
    }
    if !report.source_commits.is_empty() {
        report.pr_draft.push_str("\n## Source commits\n\n");
        for source in &report.source_commits {
            report
                .pr_draft
                .push_str(&format!("- `{}` — {}\n", source.sha, source.subject));
        }
    }
    Ok(report)
}
