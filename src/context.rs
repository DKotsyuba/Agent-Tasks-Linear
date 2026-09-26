//! Read-only agent views assembled from native work, persisted reports and activity.

use crate::{
    activity::ActivityRecord,
    model::{Fault, Kind, Result, Status, Work, require},
    reports::{ModuleReport, module_report},
    rules,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, VecDeque},
    time::{Duration, Instant},
};

/// Maximum retained comparison points across all Projects in one gateway process.
const MAX_BASELINES: usize = 32;
/// Comparison points expire after this interval; workflow state always comes from Linear.
const BASELINE_TTL: Duration = Duration::from_secs(30 * 60);
/// Bound each compact comparison point before it enters process memory.
const MAX_SNAPSHOT_BYTES: usize = 256 * 1024;

/// Resolve a native Linear Issue URL to its human identifier without fetching the URL.
/// Only HTTPS links on linear.app with an issue path are accepted; a malformed or unrelated
/// reference returns INVALID_LINK. The caller still resolves the identifier through Linear.
pub fn issue_identifier(reference: &str) -> Result<String> {
    let url = reqwest::Url::parse(reference)
        .map_err(|_| Fault::new("INVALID_LINK", "Expected a native Linear Issue URL"))?;
    let parts: Vec<_> = url
        .path_segments()
        .map(|segments| segments.collect())
        .unwrap_or_default();
    let identifier = parts
        .windows(2)
        .find_map(|pair| (pair[0] == "issue").then_some(pair[1]))
        .unwrap_or("");
    require(
        url.scheme() == "https"
            && url.host_str() == Some("linear.app")
            && url.query().is_none()
            && url.fragment().is_none()
            && identifier.contains('-')
            && identifier
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "INVALID_LINK",
        "Expected a native Linear Issue URL",
    )?;
    Ok(identifier.to_owned())
}

/// Collect native drift from a Module and every direct child before any derived count is shown.
/// A child's pending write, status mismatch or missing metadata makes its apparent native Done
/// status provisional, even when the parent itself has no discrepancy.
pub fn module_discrepancies(module: &Work, graph: &[Work]) -> Vec<String> {
    let mut problems = rules::discrepancies(module, graph);
    for child in rules::children(graph, module.id()) {
        let label = child.native["identifier"].as_str().unwrap_or(child.id());
        problems.extend(
            rules::discrepancies(child, graph)
                .into_iter()
                .map(|problem| format!("{label}: {problem}")),
        );
    }
    problems
}

/// Render the assignment and evidence for one managed Issue without loading external data.
/// The graph is the complete native Project hierarchy; `activity` holds bounded native comment
/// reads keyed by Issue UUID, and `documents` contains metadata links only. An incomplete native
/// membership is exposed in `discrepancies`, with no derived Module count presented as exact.
pub fn agent_context(
    work: &Work,
    graph: &[Work],
    view: &str,
    activity: &BTreeMap<String, Vec<ActivityRecord>>,
    documents: &[Value],
    report: Option<&ModuleReport>,
) -> Result<Value> {
    require(
        matches!(view, "lead" | "reviewer"),
        "INVALID_INPUT",
        "view must be lead or reviewer",
    )?;
    let meta = work.managed()?;
    let parent = rules::parent(work).and_then(|id| rules::find(graph, id));
    let module = if meta.kind == Kind::Module {
        Some(work)
    } else if parent.is_some_and(|item| item.meta.as_ref().is_some_and(|m| m.kind == Kind::Module))
    {
        parent
    } else {
        None
    };
    let epic = if meta.kind == Kind::Epic {
        Some(work)
    } else {
        module
            .and_then(|item| rules::parent(item))
            .or_else(|| {
                parent
                    .filter(|item| item.meta.as_ref().is_some_and(|m| m.kind == Kind::Epic))
                    .map(Work::id)
            })
            .and_then(|id| rules::find(graph, id))
    };
    let mut children = rules::children(graph, work.id());
    children.sort_by(|a, b| crate::gateway::priority_cmp(&a.native, &b.native));
    let tasks: Vec<_> = children
        .iter()
        .map(|child| {
            json!({"id":child.id(),"identifier":child.native["identifier"],"title":child.native["title"],
                "url":child.native["url"],"status":child.native["state"]["name"],"priority":child.native["priority"],
                "result":child.fields["result"],"reported_checks":child.fields["check_result"],
                "git_reports":child.meta.as_ref().map(|m| m.current_git_reports().collect::<Vec<_>>())})
        })
        .collect();
    let ids = std::iter::once(work.id()).chain(children.iter().map(|child| child.id()));
    let open_questions: Vec<_> = ids
        .filter_map(|id| activity.get(id))
        .flat_map(|records| records.iter())
        .filter(|record| record.kind == "question" && record.resolved_at.is_none())
        .map(|record| json!({"id":record.id,"url":record.url,"target":record.target,"recipient":record.recipient,"body":record.body}))
        .collect();
    let latest_review = activity.get(work.id()).and_then(|records| {
        records
            .iter()
            .filter(|record| record.formal_review)
            .max_by(|a, b| a.created_at.cmp(&b.created_at))
    });
    let discrepancies = if meta.kind == Kind::Module {
        module_discrepancies(work, graph)
    } else {
        rules::discrepancies(work, graph)
    };
    let checkout = module.map(|item| json!({"repository_path":item.fields["repository_path"],
        "branch":item.fields["branch"],"worktree":item.fields["worktree"],"lead":item.fields["lead"]}));
    let links: Vec<_> = documents
        .iter()
        .map(|doc| json!({"id":doc["id"],"title":doc["title"],"url":doc["url"]}))
        .collect();
    Ok(json!({
        "view":view,"id":work.id(),"url":work.native["url"],"identifier":work.native["identifier"],
        "title":work.native["title"],"kind":meta.kind,"status":work.native["state"]["name"],
        "goal":work.fields["expected_result"],"description":work.fields["description"],
        "acceptance_criteria":work.fields["acceptance_criteria"],
        "required_contract":module.map(|item| &item.fields["required_contract"]),
        "provided_contract":module.map(|item| &item.fields["provided_contract"]),
        "epic":epic.map(|item| json!({"id":item.id(),"url":item.native["url"],"business_requirements":item.fields["business_requirements"],"acceptance_criteria":item.fields["acceptance_criteria"]})),
        "checkout":checkout,"tasks":tasks,"module_report":report,"current_git_reports":meta.current_git_reports().collect::<Vec<_>>(),
        "latest_review":latest_review,"open_questions":open_questions,"documents":links,"discrepancies":discrepancies,
        "next_work":if view == "lead" {json!(tasks.iter().filter(|task| !matches!(task["status"].as_str(),Some("Done"|"Canceled"|"Duplicate"))).collect::<Vec<_>>())} else {Value::Null},
        "review_evidence":if view == "reviewer" {json!({"module_report":report,"latest_review":latest_review})} else {Value::Null}
    }))
}

/// Compose one Module card from the shared current-round report and native assignment.
/// A discrepancy makes the whole overview incomplete rather than silently reducing its totals.
fn module_card(module: &Work, graph: &[Work]) -> Result<Value> {
    let problems = module_discrepancies(module, graph);
    require(
        problems.is_empty(),
        "INCOMPLETE_DATA",
        format!(
            "{}: {}",
            module.native["identifier"].as_str().unwrap_or(module.id()),
            problems.join("; ")
        ),
    )?;
    let report = module_report(module, graph)?;
    Ok(
        json!({"id":module.id(),"identifier":module.native["identifier"],"url":module.native["url"],
        "title":module.native["title"],"status":module.native["state"]["name"],
        "priority":module.native["priority"],"lead":module.fields["lead"],
        "session_url":module.fields["session_url"],"expected_result":module.fields["expected_result"],
        "report":report}),
    )
}

/// Build a complete read-only Project overview from one native graph and bounded activity reads.
/// Active Epics show every frozen Module, while active root Modules and all Atomics are separate.
/// Missing metadata, recorded children or status drift fails with INCOMPLETE_DATA rather than
/// reporting false progress. Activity identifies open questions and current review evidence.
pub fn project_overview(
    project: &Value,
    graph: &[Work],
    activity: &BTreeMap<String, Vec<ActivityRecord>>,
) -> Result<Value> {
    for work in graph.iter().filter(|work| work.meta.is_some()) {
        work.status()?;
    }
    let mut epics: Vec<_> = graph
        .iter()
        .filter(|work| {
            work.meta.as_ref().is_some_and(|m| m.kind == Kind::Epic)
                && matches!(work.status(), Ok(Status::InProgress | Status::InReview))
        })
        .collect();
    epics.sort_by(|a, b| crate::gateway::priority_cmp(&a.native, &b.native));
    let mut epic_cards = Vec::new();
    let mut draft = format!(
        "## Project overview\n\n{}\n\n",
        project["name"].as_str().unwrap_or("Project")
    );
    for epic in epics {
        let problems = rules::discrepancies(epic, graph);
        require(
            problems.is_empty(),
            "INCOMPLETE_DATA",
            format!("{}: {}", epic.id(), problems.join("; ")),
        )?;
        let frozen = epic.managed()?.frozen_modules.as_ref().ok_or_else(|| {
            Fault::new(
                "INCOMPLETE_DATA",
                "Active Epic has no frozen Module membership",
            )
        })?;
        let mut modules: Vec<_> = frozen
            .iter()
            .map(|id| {
                let module = rules::find(graph, id).ok_or_else(|| {
                    Fault::new(
                        "INCOMPLETE_DATA",
                        "Frozen Module is missing from the Project graph",
                    )
                })?;
                require(
                    module.managed()?.kind == Kind::Module,
                    "INCOMPLETE_DATA",
                    "Frozen member is not a Module",
                )?;
                Ok(module)
            })
            .collect::<Result<_>>()?;
        modules.sort_by(|a, b| crate::gateway::priority_cmp(&a.native, &b.native));
        let cards: Vec<Value> = modules
            .iter()
            .map(|module| module_card(module, graph))
            .collect::<Result<_>>()?;
        let done: usize = cards
            .iter()
            .filter(|card| !matches!(card["status"].as_str(), Some("Canceled" | "Duplicate")))
            .map(|card| card["report"]["tasks_done"].as_u64().unwrap_or(0) as usize)
            .sum();
        let total: usize = cards
            .iter()
            .filter(|card| !matches!(card["status"].as_str(), Some("Canceled" | "Duplicate")))
            .map(|card| card["report"]["tasks_total"].as_u64().unwrap_or(0) as usize)
            .sum();
        draft.push_str(&format!(
            "### [{}]({}) — {done}/{total} Tasks Done\n\n",
            epic.native["title"].as_str().unwrap_or("Epic"),
            epic.native["url"].as_str().unwrap_or("")
        ));
        for card in &cards {
            draft.push_str(&format!(
                "- [{}]({}): {}, {}/{} Tasks Done; lead {}\n",
                card["identifier"].as_str().unwrap_or("Module"),
                card["url"].as_str().unwrap_or(""),
                card["status"].as_str().unwrap_or("unknown"),
                card["report"]["tasks_done"],
                card["report"]["tasks_total"],
                card["lead"].as_str().unwrap_or("unassigned")
            ));
        }
        draft.push('\n');
        epic_cards.push(json!({"id":epic.id(),"identifier":epic.native["identifier"],"url":epic.native["url"],
            "title":epic.native["title"],"status":epic.native["state"]["name"],"priority":epic.native["priority"],
            "business_requirements":epic.fields["business_requirements"],"expected_result":epic.fields["expected_result"],
            "result":epic.fields["result"],"modules":cards,"tasks_done":done,"tasks_total":total}));
    }
    let mut standalone: Vec<_> = graph
        .iter()
        .filter(|work| {
            work.meta.as_ref().is_some_and(|m| m.kind == Kind::Module)
                && rules::parent(work).is_none()
                && matches!(work.status(), Ok(Status::InProgress | Status::InReview))
        })
        .collect();
    standalone.sort_by(|a, b| crate::gateway::priority_cmp(&a.native, &b.native));
    let standalone: Vec<Value> = standalone
        .into_iter()
        .map(|module| module_card(module, graph))
        .collect::<Result<_>>()?;
    if !standalone.is_empty() {
        draft.push_str("### Standalone Modules\n\n");
        for card in &standalone {
            draft.push_str(&format!(
                "- [{}]({}): {}, {}/{} Tasks Done; lead {}\n",
                card["identifier"].as_str().unwrap_or("Module"),
                card["url"].as_str().unwrap_or(""),
                card["status"].as_str().unwrap_or("unknown"),
                card["report"]["tasks_done"],
                card["report"]["tasks_total"],
                card["lead"].as_str().unwrap_or("unassigned")
            ));
        }
        draft.push('\n');
    }
    let mut atomic_work: Vec<_> = graph
        .iter()
        .filter(|work| work.meta.as_ref().is_some_and(|m| m.kind == Kind::Atomic))
        .collect();
    atomic_work.sort_by(|a, b| crate::gateway::priority_cmp(&a.native, &b.native));
    let atomics: Vec<_> = atomic_work.into_iter()
        .map(|work| json!({"id":work.id(),"identifier":work.native["identifier"],"url":work.native["url"],
            "title":work.native["title"],"status":work.native["state"]["name"],"priority":work.native["priority"],
            "result":work.fields["result"],"reported_checks":work.fields["check_result"]})).collect();
    let mut excluded_work: Vec<_> = graph
        .iter()
        .filter(|work| matches!(work.status(), Ok(Status::Canceled | Status::Duplicate)))
        .collect();
    excluded_work.sort_by_key(|work| work.id());
    let excluded: Vec<_> = excluded_work
        .into_iter()
        .map(|work| {
            json!({"id":work.id(),"identifier":work.native["identifier"],"url":work.native["url"],
            "status":work.native["state"]["name"],"reason":work.fields["reason"]})
        })
        .collect();
    let awaiting_review: Vec<_> = graph.iter().filter(|work| matches!(work.status(), Ok(Status::InReview)))
        .map(|work| json!({"id":work.id(),"identifier":work.native["identifier"],"url":work.native["url"],
            "review":activity.get(work.id()).and_then(|records| records.iter().find(|record| record.formal_review))})).collect();
    let mut open_questions: Vec<_> = activity
        .values()
        .flat_map(|records| records.iter())
        .filter(|record| record.kind == "question" && record.resolved_at.is_none())
        .collect();
    open_questions.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    let questions: Vec<_> = open_questions.into_iter()
        .map(|record| json!({"id":record.id,"url":record.url,"target":record.target,"recipient":record.recipient,"body":record.body})).collect();
    if !questions.is_empty() {
        draft.push_str("### Open questions\n\n");
        for question in &questions {
            draft.push_str(&format!(
                "- [{}]({})\n",
                question["body"].as_str().unwrap_or("Question"),
                question["url"].as_str().unwrap_or("")
            ));
        }
    }
    require(
        draft.chars().count() <= 30_000,
        "REPORT_LIMIT",
        "ProjectUpdate draft exceeds the native body limit",
    )?;
    Ok(
        json!({"project_id":project["id"],"project_title":project["name"],"project_url":project["url"],
        "active_epics":epic_cards,"standalone_modules":standalone,"atomics":atomics,"excluded":excluded,
        "awaiting_review":awaiting_review,"open_questions":questions,"project_update_draft":draft}),
    )
}

/// Hash one current value so a compact snapshot still detects changes beyond its preview.
fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

/// Keep a short human hint alongside a digest; absent values stay absent rather than empty.
fn preview(value: Option<&str>) -> Option<String> {
    value.map(|text| text.chars().take(240).collect())
}

/// Capture only the fields whose change matters to a repeated Project overview.
/// `project` contributes displayed identity; work includes completion, assignments, field digests
/// and current-round source/review references. Activity includes questions, replies, resolution and formal review state. The
/// returned map is deterministic and never serves as authoritative workflow state.
pub fn compact_snapshot(
    project: &Value,
    graph: &[Work],
    activity: &BTreeMap<String, Vec<ActivityRecord>>,
) -> Result<BTreeMap<String, Value>> {
    let mut snapshot = BTreeMap::new();
    snapshot.insert(
        "project".into(),
        json!({"id":project["id"],"name":project["name"],"url":project["url"]}),
    );
    for work in graph.iter().filter(|work| work.meta.is_some()) {
        let meta = work.managed()?;
        let result = work.fields["result"].as_str();
        let checks = work.fields["check_result"].as_str();
        snapshot.insert(format!("work:{}", work.id()), json!({
            "id":work.id(),"url":work.native["url"],"identifier":work.native["identifier"],
            "title":work.native["title"],"priority":work.native["priority"],"parent_id":work.native["parent"]["id"],
            "kind":meta.kind,"status":work.status()?,"lead":work.fields["lead"],
            "round":meta.round,"revision":meta.revision,
            "fields_hash":digest(&serde_json::to_string(&work.fields).unwrap_or_default()),
            "result_hash":result.map(digest),"result_preview":preview(result),
            "checks_hash":checks.map(digest),
            "source_commits":meta.current_git_reports().map(|report| json!({"sha":report.commit.sha,"repository_identity":report.commit.repository_identity})).collect::<Vec<_>>(),
            "review":meta.review
        }));
    }
    for records in activity.values() {
        for record in records {
            snapshot.insert(format!("activity:{}", record.id), json!({
                "id":record.id,"url":record.url,"target":record.target,"kind":record.kind,
                "parent_id":record.parent_id,"recipient":record.recipient,
                "record_hash":digest(&serde_json::to_string(record).unwrap_or_default()),
                "body_hash":digest(&record.body),"body_preview":preview(Some(&record.body)),
                "created_at":record.created_at,"updated_at":record.updated_at,
                "resolved_at":record.resolved_at,"resolving_comment_id":record.resolving_comment_id,
                "verdict":record.verdict,"findings_hash":record.findings.as_deref().map(digest),
                "formal_review":record.formal_review,"health":record.health,"reason":record.reason
            }));
        }
    }
    require(
        serde_json::to_vec(&snapshot)
            .map(|bytes| bytes.len())
            .unwrap_or(usize::MAX)
            <= MAX_SNAPSHOT_BYTES,
        "INCOMPLETE_DATA",
        "Comparison snapshot exceeds 256 KiB",
    )?;
    Ok(snapshot)
}

/// One opaque previous observation bound to its Project and process lifetime.
struct Baseline {
    /// Random cursor returned to the caller; it carries no Project data itself.
    cursor: String,
    /// Native Project UUID; a cursor from another Project is never compared.
    project_id: String,
    /// Monotonic process time used only for bounded expiry.
    captured_at: Instant,
    /// Compact prior fields, never an authoritative work record.
    snapshot: BTreeMap<String, Value>,
}

/// Small process-local comparison cache; restarts and eviction intentionally lose baselines.
#[derive(Default)]
pub struct SnapshotCache {
    /// Oldest-first observations, capped at MAX_BASELINES.
    entries: VecDeque<Baseline>,
}

impl SnapshotCache {
    /// Compare with a valid same-Project cursor, then retain the current compact snapshot.
    /// An absent cursor requests a full overview; expired, foreign or unknown cursors produce
    /// `baseline_expired=true` and no changes array, so callers must use the full result.
    /// `now` is monotonic process time, injectable for deterministic expiry checks.
    pub fn compare(
        &mut self,
        project_id: &str,
        cursor: Option<&str>,
        snapshot: BTreeMap<String, Value>,
        now: Instant,
    ) -> Value {
        self.entries.retain(|entry| {
            now.checked_duration_since(entry.captured_at)
                .unwrap_or_default()
                < BASELINE_TTL
        });
        let old = cursor.and_then(|key| {
            self.entries
                .iter()
                .find(|entry| entry.cursor == key && entry.project_id == project_id)
        });
        let changes = old.map(|entry| {
            let mut changes = Vec::new();
            for (key, after) in &snapshot {
                if entry.snapshot.get(key) != Some(after) {
                    changes.push(json!({"key":key,"before":entry.snapshot.get(key),"after":after}));
                }
            }
            for (key, before) in &entry.snapshot {
                if !snapshot.contains_key(key) {
                    changes.push(json!({"key":key,"before":before,"after":Value::Null}));
                }
            }
            changes
        });
        let baseline_expired = cursor.is_some() && changes.is_none();
        if self.entries.len() == MAX_BASELINES {
            self.entries.pop_front();
        }
        let next = uuid::Uuid::new_v4().to_string();
        self.entries.push_back(Baseline {
            cursor: next.clone(),
            project_id: project_id.into(),
            captured_at: now,
            snapshot,
        });
        json!({"cursor":next,"baseline_expired":baseline_expired,"changes":changes})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Capacity, scope and monotonic expiry never turn a missing baseline into no changes.
    #[test]
    fn bounded_baselines_expire_explicitly() {
        let mut cache = SnapshotCache::default();
        let now = Instant::now();
        let snapshot = BTreeMap::from([("work:a".into(), json!({"status":"In Progress"}))]);
        let first = cache.compare("project-a", None, snapshot.clone(), now);
        let unchanged = cache.compare("project-a", first["cursor"].as_str(), snapshot.clone(), now);
        assert_eq!(unchanged["changes"], json!([]));
        assert_eq!(unchanged["baseline_expired"], false);
        for _ in 0..MAX_BASELINES {
            cache.compare("project-a", None, snapshot.clone(), now);
        }
        assert_eq!(cache.entries.len(), MAX_BASELINES);
        let evicted = cache.compare("project-a", first["cursor"].as_str(), snapshot.clone(), now);
        assert_eq!(evicted["baseline_expired"], true);
        assert!(evicted["changes"].is_null());
        let foreign = cache.compare(
            "project-b",
            evicted["cursor"].as_str(),
            snapshot.clone(),
            now,
        );
        assert_eq!(foreign["baseline_expired"], true);
        assert!(foreign["changes"].is_null());
        let expired = cache.compare(
            "project-a",
            evicted["cursor"].as_str(),
            snapshot,
            now + BASELINE_TTL + Duration::from_secs(1),
        );
        assert_eq!(expired["baseline_expired"], true);
        assert!(expired["changes"].is_null());
    }
}
