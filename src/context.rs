//! Read-only agent views assembled from native work, persisted reports and activity.

use crate::{
    activity::ActivityRecord,
    model::{Fault, Kind, Result, Status, Work, require},
    reports::{ModuleReport, module_report},
    rules,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

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
    let discrepancies = rules::discrepancies(work, graph);
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
    let problems = rules::discrepancies(module, graph);
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
