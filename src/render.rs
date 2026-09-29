//! Pure, embedded MiniJinja presentation for every agent-facing MCP outcome.

use crate::{
    model::Outcome,
    records::{patch_description, read_fields},
};
use minijinja::{AutoEscape, Environment, UndefinedBehavior};
use serde_json::{Value, json};
use std::sync::OnceLock;

/// Load the closed set of embedded plain-text templates once; invalid assets trigger safe fallback.
fn environment() -> Option<&'static Environment<'static>> {
    static ENVIRONMENT: OnceLock<Option<Environment>> = OnceLock::new();
    ENVIRONMENT
        .get_or_init(|| {
            let mut env = Environment::new();
            env.set_trim_blocks(true);
            env.set_lstrip_blocks(true);
            env.set_keep_trailing_newline(true);
            env.set_undefined_behavior(UndefinedBehavior::Strict);
            env.set_auto_escape_callback(|_| AutoEscape::None);
            for (name, source) in [
                ("common", include_str!("../assets/mcp/common.txt.j2")),
                ("ack", include_str!("../assets/mcp/ack.txt.j2")),
                ("context", include_str!("../assets/mcp/context.txt.j2")),
                ("overview", include_str!("../assets/mcp/overview.txt.j2")),
                ("list", include_str!("../assets/mcp/list.txt.j2")),
                ("comment", include_str!("../assets/mcp/comment.txt.j2")),
                ("error", include_str!("../assets/mcp/error.txt.j2")),
            ] {
                if env.add_template(name, source).is_err() {
                    return None;
                }
            }
            Some(env)
        })
        .as_ref()
}

/// Map all 25 public tools to the template that owns their result layout.
fn template_for(tool: &str) -> Option<&'static str> {
    Some(match tool {
        "create_project"
        | "edit_project"
        | "create_epic"
        | "edit_epic"
        | "create_module"
        | "edit_module"
        | "create_task"
        | "edit_task"
        | "create_atomic"
        | "edit_atomic"
        | "save_document"
        | "upload_file"
        | "get_file"
        | "move_status"
        | "record_review"
        | "record_commits"
        | "add_comment"
        | "resolve_comment"
        | "save_project_update" => "ack",
        "get_context" => "context",
        "get_overview" => "overview",
        "list_items" | "search" | "list_files" => "list",
        "get_comment" => "comment",
        _ => return None,
    })
}

/// Render one validated tool request and immutable Gateway outcome without I/O or mutation.
/// Confirmed outcomes remain confirmed when projection or template rendering fails.
pub fn render_outcome(tool: &str, request: &Value, outcome: &Outcome) -> String {
    let success = matches!(outcome.status.as_str(), "ok" | "committed" | "noop");
    if success && !valid_success_shape(tool, request, &outcome.data) {
        return presentation_fallback(tool, request, outcome);
    }
    let (template, context) = if success {
        let projection = match tool {
            "get_context" => context_projection(request, &outcome.data),
            "get_overview" => overview_projection(&outcome.data),
            "list_items" | "search" | "list_files" => list_projection(tool, request, &outcome.data),
            "get_comment" => comment_projection(&outcome.data),
            _ => ack_projection(tool, request, &outcome.data),
        };
        (template_for(tool).unwrap_or("missing"), projection)
    } else {
        (
            "error",
            json!({
                "status":outcome.status,
                "code":outcome.data["code"].as_str().unwrap_or("TOOL_FAILED"),
                "message":outcome.data["message"].as_str().unwrap_or("Request failed"),
                "retry":outcome.data["retry"].as_str().unwrap_or(""),
                "request_id":request["request_id"].as_str().unwrap_or(""),
                "invalid_commit":outcome.data["code"] == "INVALID_COMMIT_MESSAGE"
            }),
        )
    };
    render_template(template, context)
        .unwrap_or_else(|_| presentation_fallback(tool, request, outcome))
}

/// Reject essential missing read fields before an otherwise empty success page can be emitted.
/// An omitted `type` infers the same envelopes the resolver selected from a native URL.
fn valid_success_shape(tool: &str, request: &Value, data: &Value) -> bool {
    match tool {
        "get_context" => match request["type"].as_str() {
            Some("document") => data["id"].is_string() && data["content"].is_string(),
            Some("project") => data["project"]["id"].is_string(),
            Some("project_update") => data["project_update"]["id"].is_string(),
            Some("issue") => data["issue"]["id"].is_string(),
            _ => {
                data["issue"]["id"].is_string()
                    || data["project"]["id"].is_string()
                    || data["project_update"]["id"].is_string()
                    || (data["id"].is_string() && data["content"].is_string())
            }
        },
        "get_overview" => data["project_id"].is_string() && data["cursor"].is_string(),
        "list_items" | "search" | "list_files" => {
            data["nodes"].is_array() && data["pageInfo"].is_object()
        }
        "get_comment" => {
            data["comment"]["id"].is_string()
                && data["activity"].is_object()
                && data["replies"]["nodes"].is_array()
        }
        _ => data.is_object(),
    }
}

/// Resolve and execute one embedded template; the caller owns status-preserving fallback.
fn render_template(name: &str, context: Value) -> Result<String, String> {
    environment()
        .ok_or("template registration failed")?
        .get_template(name)
        .map_err(|error| error.to_string())?
        .render(context)
        .map_err(|error| error.to_string())
}

/// Keep confirmed status and the best known ID when the presentation layer fails.
fn presentation_fallback(tool: &str, request: &Value, outcome: &Outcome) -> String {
    let target = outcome.data["id"]
        .as_str()
        .or_else(|| outcome.data["issue"]["id"].as_str())
        .or_else(|| outcome.data["project"]["id"].as_str())
        .or_else(|| outcome.data["project_update"]["id"].as_str())
        .or_else(|| outcome.data["comment"]["id"].as_str())
        .or_else(|| outcome.data["review"]["id"].as_str())
        .or_else(|| request["id"].as_str())
        .or_else(|| request["work_id"].as_str())
        .or_else(|| request["project_id"].as_str())
        .or_else(|| request["target_id"].as_str())
        .or_else(|| request["url"].as_str())
        .unwrap_or("");
    let kind = request["type"]
        .as_str()
        .or_else(|| request["target_type"].as_str())
        .or_else(|| envelope_kind(&outcome.data))
        .unwrap_or_else(|| fallback_kind(tool));
    let request_id = request["request_id"].as_str().unwrap_or("");
    if matches!(outcome.status.as_str(), "ok" | "committed" | "noop") {
        format!(
            "{tool}: {}. target_type: {kind}; target: {target}; request_id: {request_id}. Presentation failed after the operation; inspect context before retrying a mutation.",
            outcome.status
        )
    } else {
        format!(
            "{}: {}. target_type: {kind}; target: {target}; request_id: {request_id}. Presentation failed; inspect context before retrying.",
            outcome.status,
            outcome.data["code"].as_str().unwrap_or("TOOL_FAILED")
        )
    }
}

/// Infer the public entity type from a native success envelope before the tool default.
fn envelope_kind(data: &Value) -> Option<&'static str> {
    ["issue", "project", "project_update", "comment", "review"]
        .into_iter()
        .find(|key| data[*key].is_object())
}

/// Infer the public entity type for retry guidance when a mutation request has no type field.
fn fallback_kind(tool: &str) -> &'static str {
    match tool {
        "create_project" | "edit_project" | "get_overview" => "project",
        "save_document" => "document",
        "upload_file" | "get_file" | "list_files" => "file",
        "get_comment" | "resolve_comment" => "comment",
        "save_project_update" => "project_update",
        "add_comment" => "",
        _ => "issue",
    }
}

/// Select native identity fields as values; templates own their labels and order.
/// The trailing group is the flat file envelope agreed for upload_file/list_files/get_file;
/// it is null for every other tool's item and so never renders through `line`.
fn identity(item: &Value) -> Value {
    json!({
        "id":item["id"],"identifier":item["identifier"],
        "title":if item["title"].is_string() {&item["title"]} else {&item["name"]},
        "url":item["url"],"status":item["state"]["name"],
        "priority":item["priority"],"health":item["health"],
        "updated_at":item["updatedAt"],"resolved":item["resolvedAt"].is_string(),
        "project_id":item["project"]["id"],"team_id":item["team"]["id"],
        "parent_id":item["parent"]["id"],
        "work_id":item["work_id"],"work_url":item["work_url"],
        "file_name":item["file_name"],"content_type":item["content_type"],
        "size_bytes":item["size_bytes"],"path":item["path"]
    })
}

/// Select mutation confirmation data without mirroring the submitted body.
fn ack_projection(tool: &str, request: &Value, data: &Value) -> Value {
    let native = if matches!(tool, "save_document" | "upload_file" | "get_file") {
        data
    } else {
        ["issue", "project", "project_update", "comment", "review"]
            .iter()
            .find_map(|key| data.get(*key))
            .unwrap_or(data)
    };
    let commits: Vec<Value> = data["git_reports"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|report| {
            let commit = if report["commit"].is_object() {
                &report["commit"]
            } else {
                report
            };
            json!({"sha":commit["sha"],"subject":commit["subject"]})
        })
        .collect();
    json!({"tool":tool,"request":request,"data":data,"item":identity(native),
        "commits":commits,"documents":data["documents"].as_array().cloned().unwrap_or_default(),
        "journal":data["journal"].as_array().cloned().unwrap_or_default(),
        "check_only":tool=="move_status" && data.get("allowed").is_some(),
        "review_url":data["url"]})
}

/// Select the requested entity before inspecting its native relationship objects.
fn context_projection(request: &Value, data: &Value) -> Value {
    if data["detail"] == "brief" {
        // One compact current slice: state, assignment, results, handoff, document links,
        // recovery payload and explicit routes to the complete content and archive.
        let brief_item = if data["issue"].is_object() {
            &data["issue"]
        } else {
            &data["project"]
        };
        let mut item = identity(brief_item);
        item["status"] = brief_item["status"].clone();
        item["kind"] = brief_item["kind"].clone();
        item["repository_path"] = brief_item["repository_path"].clone();
        item["repository_url"] = brief_item["repository_url"].clone();
        item["teams"] = brief_item["teams"].clone();
        let pending_call = data["workflow"]["pending"]["request"]
            .as_object()
            .map(|pending| {
                serde_json::to_string_pretty(&json!({
                    "tool":pending.get("tool"),"arguments":pending.get("arguments")}))
                .expect("JSON values serialize")
            });
        return json!({"kind":"brief","item":item,"fields":data["fields"],
            "guidance":data["guidance"],
            "handoff":data["handoff"],"documents":data["documents"].as_array().cloned().unwrap_or_default(),
            "routes":data["full_context"],"runtime":data["runtime"],"pending_call":pending_call,
            "discrepancies":data["discrepancies"].as_array().cloned().unwrap_or_default(),
            "transitions":data["transitions"].as_array().cloned().unwrap_or_default()});
    }
    let kind = if request["type"] == "document" || data["id"].is_string() {
        "document"
    } else if data.get("issue").is_some() {
        "issue"
    } else if data.get("project").is_some() {
        "project"
    } else if data.get("project_update").is_some() {
        "project_update"
    } else {
        "document"
    };
    let item = match kind {
        "issue" => &data["issue"],
        "project" => &data["project"],
        "project_update" => &data["project_update"],
        _ => data,
    };
    let agent = &data["agent_context"];
    let mut checkout = if agent["checkout"].is_object() {
        agent["checkout"].clone()
    } else {
        data["parent_checkout"].clone()
    };
    if checkout.is_object() && checkout["repository_url"].is_null() {
        checkout["repository_url"] = data["parent_checkout"]["repository_url"].clone();
    }
    let children = if agent["tasks"].is_array() {
        &agent["tasks"]
    } else {
        &data["children"]
    };
    let children: Vec<Value> = children
        .as_array()
        .into_iter()
        .flatten()
        .map(|child| {
            let mut item = identity(child);
            item["status"] = if child["status"].is_string() {
                child["status"].clone()
            } else {
                child["state"]["name"].clone()
            };
            json!({"item":item,"result":child["result"],"checks":child["reported_checks"]})
        })
        .collect();
    let commits: Vec<Value> = data["git_reports"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|report| {
            let commit = if report["commit"].is_object() {
                &report["commit"]
            } else {
                report
            };
            json!({"sha":commit["sha"],"subject":commit["subject"]})
        })
        .collect();
    let pending_call = data["workflow"]["pending"]["request"]
        .as_object()
        .map(|pending| {
            serde_json::to_string_pretty(
                &json!({"tool":pending.get("tool"),"arguments":pending.get("arguments")}),
            )
            .expect("JSON values serialize")
        });
    let body = if data["activity"]["body"].is_string() {
        &data["activity"]["body"]
    } else {
        &item["body"]
    };
    let report = &data["module_report"];
    let description = item["description"].as_str().unwrap_or("");
    let description = if report.is_object()
        && data["fields"]["result"].is_string()
        && data["fields"]["result"] == report["summary"]
        && data["fields"]["check_result"] == report["reported_checks"]
        && read_fields(description).ok().is_some_and(|native| {
            native["result"] == report["summary"]
                && native["check_result"] == report["reported_checks"]
        }) {
        patch_description(description, &json!({"result":null,"check_result":null}))
    } else {
        description.to_owned()
    };
    json!({"kind":kind,"item":identity(item),"description":description,"content":item["content"],
        "body_text":body,"agent":agent,"checkout":checkout,"children":children,"commits":commits,
        "report":report,
        "epic":agent["epic"],"required_contract":if data["fields"]["required_contract"].is_string(){&Value::Null}else{&agent["required_contract"]},
        "provided_contract":if data["fields"]["provided_contract"].is_string(){&Value::Null}else{&agent["provided_contract"]},
        "review":agent["latest_review"],"questions":agent["open_questions"].as_array().cloned().unwrap_or_default(),
        "documents":if kind=="project" {data["documents"].as_array().cloned().unwrap_or_default()} else {agent["documents"].as_array().cloned().unwrap_or_default()},
        "teams":item["teams"]["nodes"].as_array().cloned().unwrap_or_default(),
        "peers":data["priority_group"]["peers"].as_array().cloned().unwrap_or_default(),
        "guidance":data["guidance"],
        "pending_call":pending_call,
        "discrepancies":data["discrepancies"].as_array().cloned().unwrap_or_default(),
        "transitions":data["transitions"].as_array().cloned().unwrap_or_default(),
        "actor":data["activity"]["actor"],"reason":data["activity"]["reason"]})
}

/// Select project cards and delta source values without printing internal hashes.
fn overview_projection(data: &Value) -> Value {
    let changes: Vec<Value> = data["changes"].as_array().into_iter().flatten().map(|change| {
        let current = if change["after"].is_null() {&change["before"]} else {&change["after"]};
        let label = current["identifier"].as_str().or_else(||current["url"].as_str()).or_else(||change["key"].as_str()).unwrap_or("");
        json!({"label":label,"url":current["url"],
            "before":snapshot_projection(&change["before"]),"after":snapshot_projection(&change["after"]),
            "removed":change["after"].is_null(),"added":change["before"].is_null()})
    }).collect();
    json!({"project_id":data["project_id"],"project_title":data["project_title"],"project_url":data["project_url"],
        "cursor":data["cursor"],"baseline_expired":data["baseline_expired"],
        "attention":data["attention"].as_array().cloned().unwrap_or_default(),
        "is_delta":data["changes"].is_array(),"changes":changes,
        "epics":data["active_epics"].as_array().cloned().unwrap_or_default(),
        "modules":data["standalone_modules"].as_array().cloned().unwrap_or_default(),
        "atomics":data["atomics"].as_array().cloned().unwrap_or_default(),
        "questions":data["open_questions"].as_array().cloned().unwrap_or_default(),
        "awaiting_review":data["awaiting_review"].as_array().cloned().unwrap_or_default(),
        "excluded":data["excluded"].as_array().cloned().unwrap_or_default(),
        "project_update_draft":data["project_update_draft"]})
}

/// Select comparable public fields and private comparison markers without formatting either.
fn snapshot_projection(snapshot: &Value) -> Value {
    json!({"status":snapshot["status"],"title":snapshot["title"],"name":snapshot["name"],
        "kind":snapshot["kind"],"parent_id":snapshot["parent_id"],"lead":snapshot["lead"],
        "priority":snapshot["priority"],"result_preview":snapshot["result_preview"],
        "body_preview":snapshot["body_preview"],"verdict":snapshot["verdict"],
        "health":snapshot["health"],"reason":snapshot["reason"],"resolved_at":snapshot["resolved_at"],
        "result_hash":snapshot["result_hash"],"body_hash":snapshot["body_hash"],
        "fields_hash":snapshot["fields_hash"],"checks_hash":snapshot["checks_hash"],
        "source_commits":snapshot["source_commits"],"review":snapshot["review"]})
}

/// Pair each native page item with its typed activity and retain native pagination verbatim.
fn list_projection(tool: &str, request: &Value, data: &Value) -> Value {
    let items: Vec<Value> = data["nodes"].as_array().into_iter().flatten().enumerate().map(|(i,item)| {
        json!({"item":identity(item),"activity":data["activity_records"].get(i).unwrap_or(&Value::Null)})
    }).collect();
    let entity_type = if tool == "list_files" {
        json!("file")
    } else {
        request["type"].clone()
    };
    json!({"tool":tool,"entity_type":entity_type,"items":items,
        "has_next":data["pageInfo"]["hasNextPage"],"cursor":data["pageInfo"]["endCursor"]})
}

/// Select one explicitly read comment, root and reply page without shortening their bodies.
fn comment_projection(data: &Value) -> Value {
    let replies: Vec<Value> = data["replies"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|reply| reply["id"] != data["comment"]["id"])
        .map(|reply| json!({"item":identity(reply),"body":reply["body"]}))
        .collect();
    let thread = if data["root"].is_object() {
        &data["root"]
    } else {
        &data["comment"]
    };
    json!({"item":identity(&data["comment"]),"activity":data["activity"],
        "root":identity(&data["root"]),"root_body":data["root"]["body"],
        "root_distinct":data["root"].is_object() && data["root"]["id"] != data["comment"]["id"],
        "thread_resolved":thread["resolvedAt"].is_string(),
        "thread_resolving_comment_id":thread["resolvingCommentId"],
        "replies":replies,"has_next":data["replies"]["pageInfo"]["hasNextPage"],
        "cursor":data["replies"]["pageInfo"]["endCursor"]})
}
#[cfg(test)]
mod tests {
    use super::*;

    /// Guard the mapping against a catalog addition that would silently lose its details.
    #[test]
    fn every_catalog_tool_has_a_template_and_realistic_shape() {
        let catalog = crate::catalog::Catalog::new().unwrap();
        assert_eq!(catalog.tools.len(), 25);
        for tool in catalog.tools {
            let name = tool["name"].as_str().unwrap();
            assert!(template_for(name).is_some(), "{name}");
            let request = json!({"request_id":"req-1","type":"issue"});
            let native = json!({"id":"issue-1","identifier":"MYT-1","title":"A task","url":"https://linear.app/example/issue/MYT-1/a-task","state":{"name":"In Progress"}});
            let file = json!({"id":"attachment-1","title":"Report","url":"https://uploads.linear.app/attachment-1","work_id":"issue-1","work_url":"https://linear.app/example/issue/MYT-1/a-task","file_name":"report.pdf","content_type":"application/pdf","size_bytes":1024,"replayed":false});
            let data = match name {
                "get_context" => {
                    json!({"issue":native,"fields":{},"transitions":[{"status":"Done","allowed":false,"conditions":["Checks required"]}]})
                }
                "get_overview" => {
                    json!({"project_id":"project-1","cursor":"opaque-1","standalone_modules":[{"id":"issue-1","identifier":"MYT-1","title":"A task","url":"https://linear.app/example/issue/MYT-1/a-task","status":"In Progress","report":{"tasks_done":0,"tasks_total":1}}]})
                }
                "list_items" | "search" => {
                    json!({"nodes":[native],"pageInfo":{"hasNextPage":true,"endCursor":"opaque-1"}})
                }
                "list_files" => {
                    json!({"nodes":[file],"pageInfo":{"hasNextPage":false,"endCursor":null}})
                }
                "get_comment" => {
                    json!({"comment":{"id":"comment-1","url":"https://linear.app/comment-1"},"activity":{"kind":"note","body":"Exact comment body"},"replies":{"nodes":[],"pageInfo":{"hasNextPage":false}}})
                }
                "create_project" => {
                    json!({"project":{"id":"project-1","name":"A project","url":"https://linear.app/project-1"},"documents":[{"title":"Runbook","url":"https://linear.app/document-1"}]})
                }
                "edit_project" => {
                    json!({"id":"project-1","name":"A project","url":"https://linear.app/project-1"})
                }
                "save_document" => {
                    json!({"id":"document-1","title":"Plan","url":"https://linear.app/document-1","content":"Do not echo this submitted body"})
                }
                "upload_file" | "get_file" => file.clone(),
                "record_review" => {
                    json!({"review":{"id":"review-1"},"url":"https://linear.app/review-1"})
                }
                "add_comment" | "resolve_comment" => {
                    json!({"comment":{"id":"comment-1","url":"https://linear.app/comment-1"},"replayed":false})
                }
                "save_project_update" => {
                    json!({"project_update":{"id":"update-1","url":"https://linear.app/update-1","health":"onTrack"},"replayed":false})
                }
                "record_commits" => {
                    json!({"issue":native,"git_reports":[{"sha":"abcdef123","subject":"feat: work"}]})
                }
                _ => json!({"issue":native,"replayed":false}),
            };
            let text = render_outcome(name, &request, &Outcome::ok(data));
            assert!(
                !text.is_empty() && !text.contains("Presentation failed"),
                "{name}: {text}"
            );
            assert!(!text.contains("\"status\":\"ok\""), "{name}");
            assert!(
                text.contains("ID:") || text.contains("Cursor:"),
                "{name}: {text}"
            );
        }
    }

    /// File operations render their own agreed fields (work item, name, type, size, path),
    /// not just the shared identity block, for both the single-item ack shape and list_files.
    #[test]
    fn file_operations_render_their_dedicated_fields() {
        let file = json!({"id":"attachment-1","title":"Check summary","url":"https://uploads.linear.app/attachment-1",
            "work_id":"issue-1","work_url":"https://linear.app/example/issue/MYT-1/a-task",
            "file_name":"check-summary.pdf","content_type":"application/pdf","size_bytes":20480,
            "replayed":false});
        for tool in ["upload_file", "get_file"] {
            let text = render_outcome(
                tool,
                &json!({"request_id":"req-1"}),
                &Outcome::ok(file.clone()),
            );
            assert!(text.contains("ID: attachment-1"), "{tool}: {text}");
            assert!(text.contains("Work ID: issue-1"), "{tool}: {text}");
            assert!(
                text.contains("Work URL: https://linear.app/example/issue/MYT-1/a-task"),
                "{tool}: {text}"
            );
            assert!(
                text.contains("File name: check-summary.pdf"),
                "{tool}: {text}"
            );
            assert!(
                text.contains("Content type: application/pdf"),
                "{tool}: {text}"
            );
            assert!(text.contains("Size bytes: 20480"), "{tool}: {text}");
            assert!(
                !text.contains("Path:"),
                "{tool}: absent path stays hidden: {text}"
            );
        }
        let mut downloaded = file.clone();
        downloaded["path"] = json!("/srv/agent/downloads/check-summary.pdf");
        let text = render_outcome(
            "get_file",
            &json!({"request_id":"req-1"}),
            &Outcome::ok(downloaded),
        );
        assert!(
            text.contains("Path: /srv/agent/downloads/check-summary.pdf"),
            "{text}"
        );

        let listed = render_outcome(
            "list_files",
            &json!({"work_id":"issue-1"}),
            &Outcome::ok(json!({"nodes":[file],"pageInfo":{"hasNextPage":false,"endCursor":null}})),
        );
        assert!(listed.contains("list_files: file"), "{listed}");
        assert!(listed.contains("File name: check-summary.pdf"), "{listed}");
        assert!(listed.contains("Content type: application/pdf"), "{listed}");
        assert!(listed.contains("Size bytes: 20480"), "{listed}");
    }

    /// Acknowledgements and previews render guidance, effect plans and overview attention.
    #[test]
    fn acknowledgements_render_guidance_effects_and_attention() {
        let acknowledged = render_outcome(
            "move_status",
            &json!({"id":"issue-1","request_id":"req-1","actor_role":"orchestrator"}),
            &Outcome::ok(
                json!({"issue":{"id":"issue-1","identifier":"MYT-1","title":"Module","url":"https://linear.app/issue-1","state":{"name":"In Review"}},
                "guidance":{"work_id":"issue-1","stage":"review",
                    "next_action":{"kind":"record_review","actor_role":"reviewer","tool":"record_review","target_status":null},
                    "conditions":[]}}),
            ),
        );
        assert!(acknowledged.contains("confirmed"), "{acknowledged}");
        assert!(acknowledged.contains("Stage: review"));
        assert!(acknowledged.contains("Next action: record_review"));
        assert!(acknowledged.contains("Next tool: record_review"));
        assert!(acknowledged.contains("Responsible role: reviewer"));

        let previewed = render_outcome(
            "move_status",
            &json!({"id":"issue-1","request_id":"req-1","actor_role":"orchestrator","check_only":true}),
            &Outcome::ok(json!({"allowed":true,"conditions":[],
                "effects":{"clears":["result","pr_url"],"review_invalidated":true,
                    "affected_integrations":["MYT-2"],"round_changes":true},
                "status":"In Progress"})),
        );
        assert!(
            previewed.contains("preview (check_only; no mutation)"),
            "{previewed}"
        );
        assert!(previewed.contains("Clears: result, pr_url"));
        assert!(previewed.contains("Review invalidated: true"));
        assert!(previewed.contains("Affected integrations: MYT-2"));
        assert!(previewed.contains("Round changes: true"));

        let overview = render_outcome(
            "get_overview",
            &json!({"project_id":"project-1"}),
            &Outcome::ok(
                json!({"project_id":"project-1","project_title":"Product","project_url":"https://linear.app/project-1",
                "cursor":"cursor-1","baseline_expired":false,
                "attention":[{"id":"issue-1","identifier":"MYT-1","url":"https://linear.app/issue-1","status":"In Review","stage":"merge",
                    "next_action":{"kind":"record_merge","actor_role":"orchestrator","tool":"edit_module","target_status":null},
                    "conditions":[]}],
                "active_epics":[],"standalone_modules":[],"atomics":[],"excluded":[],
                "awaiting_review":[],"open_questions":[],"project_update_draft":"## Project overview"}),
            ),
        );
        assert!(overview.contains("Attention: MYT-1"), "{overview}");
        assert!(overview.contains("Stage: merge"));
        assert!(overview.contains("Next action: record_merge"));
        assert!(overview.contains("Next tool: edit_module"));
    }

    /// A brief read renders one compact current slice with handoff, routes and recovery payload.
    #[test]
    fn brief_context_renders_current_slice_and_routes() {
        let brief = json!({"detail":"brief",
            "issue":{"id":"issue-1","identifier":"MYT-1","title":"A task","url":"https://linear.app/issue-1",
                "kind":"task","status":"In Progress","priority":0},
            "fields":{"result":"Implemented","check_result":"All scenarios passed","lead":"codex:lead"},
            "workflow":{"pending":{"request":{"tool":"edit_task","arguments":{"id":"issue-1"}}}},
            "discrepancies":["A write is pending; retry the same request_id and arguments"],
            "transitions":[{"status":"Done","allowed":false,"conditions":["Checks required"]}],
            "handoff":{"current":{"id":"comment-1","url":"https://linear.app/issue-1#comment-aaaaaaaa",
                "created_at":"2026-09-27T00:00:00Z","round":1,"revision":2,
                "actor":"codex:lead","body":"Continue from the resolver"},
                "revision_changed":true,"history":[]},
            "documents":[{"id":"doc-1","title":"Guide","url":"https://linear.app/doc-1","archived":false},
                {"id":"doc-2","title":"Shared plan","url":"https://linear.app/doc-2","archived":false}],
            "full_context":{"issue":"get_context type=issue id=issue-1",
                "project_documents":"list_items type=document project_id=project-1",
                "archive":"list_items type=document project_id=project-1 include_archived=true"},
            "runtime":{"version":"0.3.0","tools":22}});
        let text = render_outcome(
            "get_context",
            &json!({"type":"issue","id":"issue-1","detail":"brief"}),
            &Outcome::ok(brief),
        );
        assert!(text.contains("Brief"));
        assert!(text.contains("Status: In Progress"));
        assert!(text.contains("Kind: task"));
        assert!(text.contains("Lead: codex:lead"));
        assert!(text.contains("## Reported checks"));
        assert!(text.contains("Continue from the resolver"));
        assert!(text.contains("Revision changed: true"));
        // Brief lists only relevant own/ancestor links; archived documents stay behind the
        // explicit archive route instead of being dumped into the compact slice.
        assert!(text.contains("Document URL: https://linear.app/doc-2"));
        assert!(!text.contains("Archived:"));
        assert!(text.contains("\"tool\": \"edit_task\""));
        assert!(text.contains("A write is pending"));
        assert!(text.contains("get_context type=issue id=issue-1"));
        assert!(text.contains("include_archived=true"));
        assert!(text.contains("Server version: 0.3.0"));
        assert!(text.contains("Tools: 22"));
        assert!(!text.contains("Presentation failed"), "{text}");
    }

    /// A brief Project context renders its parsed repository path/url, native teams and the
    /// overview route, not just generic identity fields — these are easy to drop silently
    /// since the shared brief item only carries them through explicit projection keys.
    #[test]
    fn brief_project_context_renders_repository_teams_and_overview_route() {
        let brief = json!({"detail":"brief",
            "project":{"id":"project-1","name":"Passport","url":"https://linear.app/project-1",
                "repository_path":"/srv/agent/checkouts/example-product",
                "repository_url":"https://github.com/example/product",
                "teams":[{"id":"team-1","name":"Platform"}]},
            "documents":[],
            "full_context":{"project_documents":"list_items type=document project_id=project-1",
                "archive":"list_items type=document project_id=project-1 include_archived=true",
                "overview":"get_overview project_id=project-1"},
            "runtime":{"version":"0.3.0","tools":25}});
        let text = render_outcome(
            "get_context",
            &json!({"type":"project","id":"project-1","detail":"brief"}),
            &Outcome::ok(brief),
        );
        assert!(text.contains("Title: Passport"), "{text}");
        assert!(
            text.contains("Repository: /srv/agent/checkouts/example-product"),
            "{text}"
        );
        assert!(
            text.contains("Repository URL: https://github.com/example/product"),
            "{text}"
        );
        assert!(text.contains("Team ID: team-1"), "{text}");
        assert!(text.contains("Team name: Platform"), "{text}");
        assert!(
            text.contains("Overview: get_overview project_id=project-1"),
            "{text}"
        );
        assert!(!text.contains("Presentation failed"), "{text}");
    }

    /// A URL without a stated type infers its entity from the native envelope, so inferred
    /// Project, ProjectUpdate, Document and Issue reads render their real content instead of
    /// a generic fallback that relabels every target as an Issue.
    #[test]
    fn inferred_url_context_renders_every_entity_envelope() {
        let by_url = |data: Value, url: &str| {
            render_outcome(
                "get_context",
                &json!({"url":url,"request_id":"req-1"}),
                &Outcome::ok(data),
            )
        };
        let project = by_url(
            json!({"project":{"id":"project-1","name":"Agent Tasks",
                "url":"https://linear.app/example/project/agent-tasks",
                "content":"## Принятые решения\nKeep the whole project body.",
                "teams":{"nodes":[{"id":"team-1"}]}},
                "documents":[{"id":"document-1","title":"Runbook",
                "url":"https://linear.app/example/document/runbook"}]}),
            "https://linear.app/example/project/agent-tasks",
        );
        assert!(project.starts_with("Project\n"), "{project}");
        assert!(project.contains("ID: project-1"));
        assert!(project.contains("Title: Agent Tasks"));
        assert!(project.contains("Team ID: team-1"));
        assert!(project.contains("Keep the whole project body."));
        assert!(project.contains("Document URL: https://linear.app/example/document/runbook"));
        assert!(!project.contains("Presentation failed"), "{project}");

        let brief = by_url(
            json!({"detail":"brief",
                "project":{"id":"project-1","name":"Agent Tasks",
                "url":"https://linear.app/example/project/agent-tasks"},
                "documents":[{"id":"document-1","title":"Runbook",
                "url":"https://linear.app/example/document/runbook","archived":false}],
                "full_context":{"issue":null,
                "project_documents":"list_items type=document project_id=project-1",
                "archive":"list_items type=document project_id=project-1 include_archived=true"},
                "runtime":{"version":"0.3.0","tools":22}}),
            "https://linear.app/example/project/agent-tasks",
        );
        assert!(brief.starts_with("Brief"), "{brief}");
        assert!(brief.contains("Title: Agent Tasks"));
        assert!(brief.contains("include_archived=true"));

        let update = by_url(
            json!({"project_update":{"id":"c1c32217-1111-4111-8111-000000000001",
                "url":"https://linear.app/example/project/proverka/activity#project-update-c1c32217",
                "health":"atRisk","updatedAt":"2026-09-26T20:00:00Z"},
                "activity":{"actor":"codex:lead","reason":"Blocked upstream",
                "body":"The full update body."}}),
            "https://linear.app/example/project/proverka/activity#project-update-c1c32217",
        );
        assert!(update.starts_with("Project update\n"), "{update}");
        assert!(update.contains("Health: atRisk"));
        assert!(update.contains("Updated at: 2026-09-26T20:00:00Z"));
        assert!(update.contains("Reason: Blocked upstream"));
        assert!(update.contains("The full update body."), "{update}");
        assert!(!update.contains("Presentation failed"), "{update}");

        let document = by_url(
            json!({"id":"document-1","title":"Pilot plan",
                "url":"https://linear.app/example/document/pilot",
                "content":"Full document body with\nmultiple lines.",
                "issue":{"id":"issue-parent"}}),
            "https://linear.app/example/document/pilot",
        );
        assert!(document.starts_with("Document\n"), "{document}");
        assert!(document.contains("Full document body with\nmultiple lines."));

        let issue = by_url(
            json!({"issue":{"id":"issue-1","identifier":"MYT-1","title":"Readable module",
                "url":"https://linear.app/example/issue/MYT-1/readable-module",
                "project":{"id":"project-1"},"team":{"id":"team-1"},
                "parent":{"id":"module-1"},"state":{"name":"In Progress"}},
                "fields":{},"transitions":[]}),
            "https://linear.app/example/issue/MYT-1/readable-module",
        );
        assert!(issue.starts_with("Issue\n"), "{issue}");
        assert!(issue.contains("Project ID: project-1"));
        assert!(issue.contains("Team ID: team-1"));
        assert!(issue.contains("Parent ID: module-1"));

        // A failed projection still labels the inferred kind instead of defaulting to Issue.
        let fallback = presentation_fallback(
            "get_context",
            &json!({"url":"https://linear.app/example/project/agent-tasks","request_id":"req-1"}),
            &Outcome::ok(json!({"project":{"id":"project-1"}})),
        );
        assert!(
            fallback.contains("target_type: project; target: project-1"),
            "{fallback}"
        );
    }

    /// Read paths preserve human prose, full requested documents and exact recovery arguments.
    #[test]
    fn context_document_and_pending_request_keep_content() {
        let description =
            "## Known field\nRequired result\n\n## User note\nKeep this unknown section intact.";
        let args = json!({"request_id":"req-1","actor":"codex:lead","fields":{"result":"Line 1\n  line 2","flag":false,"count":0,"unset":null}});
        let issue = json!({"issue":{"id":"issue-1","identifier":"MYT-1","title":"A task","url":"https://linear.app/issue-1","project":{"id":"project-1"},"team":{"id":"team-1"},"parent":{"id":"module-1"},"state":{"name":"In Progress"},"description":description},"fields":{},"workflow":{"pending":{"request":{"tool":"edit_task","arguments":args}}},"transitions":[{"status":"Done","allowed":false,"conditions":["Checks required"]}]});
        let text = render_outcome(
            "get_context",
            &json!({"type":"issue","id":"issue-1"}),
            &Outcome::ok(issue),
        );
        assert!(text.contains(description));
        assert!(text.contains("Project ID: project-1"));
        assert!(text.contains("Team ID: team-1"));
        assert!(text.contains("Parent ID: module-1"));
        assert!(text.contains("Checks required"));
        assert!(text.contains("\"tool\": \"edit_task\""));
        assert!(text.contains("\"flag\": false"));
        assert!(text.contains("\"count\": 0"));
        assert!(text.contains("\"unset\": null"));
        assert!(text.contains("Line 1\\n  line 2"));
        assert!(!text.contains("\"before\""));
        let checkout = json!({"issue":{"id":"task-1","title":"Task","description":"Human instructions"},"agent_context":{"checkout":{"repository_path":"/repo","branch":"main","worktree":"/worktree"}},"parent_checkout":{"repository_url":"https://example.com/repo"}});
        let text = render_outcome(
            "get_context",
            &json!({"type":"issue","id":"task-1"}),
            &Outcome::ok(checkout),
        );
        assert!(text.contains("Repository URL: https://example.com/repo"));
        let document = json!({"id":"doc-1","title":"Plan","url":"https://linear.app/doc-1","content":"First line\n".to_owned()+&"Long body. ".repeat(2000)});
        let body = document["content"].as_str().unwrap();
        let rendered = render_outcome(
            "get_context",
            &json!({"type":"document","id":"doc-1"}),
            &Outcome::ok(document.clone()),
        );
        assert!(rendered.contains(body));
        assert!(rendered.len() < serde_json::to_string(&Outcome::ok(document)).unwrap().len());
        let exact = "Привет 🌍\n{\"legitimate\":true}\n{{ do_not_evaluate }}\nEND-OF-DOCUMENT";
        let rendered = render_outcome(
            "get_context",
            &json!({"type":"document","id":"doc-2"}),
            &Outcome::ok(json!({"id":"doc-2","title":"Unicode","content":exact})),
        );
        assert!(rendered.contains(exact));
        let update = json!({"project_update":{"id":"update-1","url":"https://linear.app/update-1","health":"atRisk","updatedAt":"2026-09-26T20:00:00Z"},"activity":{"actor":"codex:lead","reason":"Blocked upstream","body":"The full update body."}});
        let text = render_outcome(
            "get_context",
            &json!({"type":"project_update","id":"update-1"}),
            &Outcome::ok(update),
        );
        assert!(text.contains("Updated at: 2026-09-26T20:00:00Z"));
        assert!(text.contains("Reason: Blocked upstream"));
        assert!(text.contains("The full update body."), "{text}");
    }

    /// Native Document ownership links cannot replace the requested document or its full body.
    #[test]
    fn attached_documents_render_their_own_identity_and_content() {
        let content = format!(
            "Привет 🌍\n{{{{ do_not_evaluate }}}}\n{{\"valid\":true}}\n{}END-OF-DOCUMENT",
            "Long body. ".repeat(600)
        );
        for relation in [
            json!({"issue":{"id":"issue-parent"}}),
            json!({"project":{"id":"project-parent"}}),
        ] {
            let mut document = json!({"id":"document-1","title":"Pilot plan","url":"https://linear.app/document-1","content":content});
            document
                .as_object_mut()
                .unwrap()
                .extend(relation.as_object().unwrap().clone());
            let text = render_outcome(
                "get_context",
                &json!({"type":"document","id":"document-1"}),
                &Outcome::ok(document.clone()),
            );
            assert!(text.starts_with("Document\n"), "{text}");
            assert!(text.contains("ID: document-1"));
            assert!(text.contains(&content));
            assert!(!text.starts_with("Issue\n") && !text.starts_with("Project\n"));
            let ack = render_outcome(
                "save_document",
                &json!({"request_id":"document-1"}),
                &Outcome::ok(document),
            );
            assert!(ack.contains("ID: document-1"), "{ack}");
            assert!(
                !ack.lines()
                    .any(|line| line == "ID: issue-parent" || line == "ID: project-parent")
            );
            assert!(!ack.contains("END-OF-DOCUMENT"));
        }
    }

    /// List and overview cursors stay exact while deltas show changed values instead of hashes.
    #[test]
    fn pages_and_deltas_are_actionable() {
        let cursor = "{\"version\":1,\"last\":\"a\\\"b\"}";
        let page = json!({"nodes":[{"id":"issue-1","identifier":"MYT-1","title":"First","url":"https://linear.app/issue-1","priority":0}],"pageInfo":{"hasNextPage":true,"endCursor":cursor}});
        let text = render_outcome("list_items", &json!({"type":"issue"}), &Outcome::ok(page));
        assert!(text.contains(cursor));
        assert!(text.contains("Has next page: true"), "{text}");
        assert!(text.contains("Priority: 0"));
        let delta = json!({"project_id":"project-1","cursor":"next-1","changes":[{"key":"work:issue-1","before":{"status":"Todo","result_hash":"abc","result_preview":"Old"},"after":{"id":"issue-1","identifier":"MYT-1","url":"https://linear.app/issue-1","status":"Done","result_hash":"def","result_preview":"New"}}]});
        let text = render_outcome(
            "get_overview",
            &json!({"project_id":"project-1"}),
            &Outcome::ok(delta),
        );
        assert!(text.contains("Status: Done"));
        assert!(text.contains("Result: New"));
        assert!(text.contains("Cursor: next-1"));
        assert!(!text.contains("result_hash"));
    }

    /// Explicit activity reads keep every returned body while exposing the next reply page.
    #[test]
    fn comments_keep_full_thread_text() {
        let comment = json!({"comment":{"id":"reply-1","url":"https://linear.app/reply-1"},"activity":{"kind":"question","actor":"codex:lead","recipient":"owner","body":"Full question\nsecond line"},"root":{"id":"root-1","url":"https://linear.app/root-1","body":"Original question"},"replies":{"nodes":[{"id":"reply-2","url":"https://linear.app/reply-2","body":"Answer\nsecond line"}],"pageInfo":{"hasNextPage":true,"endCursor":"reply-cursor"}}});
        let text = render_outcome(
            "get_comment",
            &json!({"id":"reply-1"}),
            &Outcome::ok(comment),
        );
        for expected in [
            "Full question\nsecond line",
            "Original question",
            "Answer\nsecond line",
            "Next cursor: reply-cursor",
            "Recipient: owner",
        ] {
            assert!(text.contains(expected));
        }
        let page = json!({"nodes":[{"id":"root-1","url":"https://linear.app/root-1"}],"activity_records":[{"kind":"question","actor":"codex:lead","role":"reviewer","session":"https://example.com/session","source_links":["https://example.com/source"],"resolved_at":"2026-09-26T00:00:00Z","resolving_comment_id":"reply-1","body":"Unclipped list body"}],"pageInfo":{"hasNextPage":false}});
        let text = render_outcome("list_items", &json!({"type":"comment"}), &Outcome::ok(page));
        assert!(text.contains("Unclipped list body"));
        assert!(text.contains("Has next page: false"), "{text}");
        for expected in [
            "Role: reviewer",
            "Session: https://example.com/session",
            "Source: https://example.com/source",
            "Resolved: true",
            "Resolving comment ID: reply-1",
        ] {
            assert!(text.contains(expected), "{text}");
        }
    }

    /// A selected reply is shown once even if the native sibling page includes it.
    #[test]
    fn selected_reply_and_stale_terminal_cursors_are_not_repeated() {
        let thread = json!({"comment":{"id":"reply-1","url":"https://linear.app/reply-1","parent":{"id":"root-1"}},"activity":{"kind":"note","body":"Selected reply body"},"root":{"id":"root-1","url":"https://linear.app/root-1","body":"Root body"},"replies":{"nodes":[{"id":"reply-1","url":"https://linear.app/reply-1","body":"Selected reply body"},{"id":"reply-2","url":"https://linear.app/reply-2","body":"Other reply body"}],"pageInfo":{"hasNextPage":false,"endCursor":"stale-comment-cursor"}}});
        let text = render_outcome(
            "get_comment",
            &json!({"id":"reply-1"}),
            &Outcome::ok(thread),
        );
        assert_eq!(text.matches("ID: reply-1").count(), 1, "{text}");
        assert_eq!(text.matches("Selected reply body").count(), 1, "{text}");
        assert!(text.contains("Root body") && text.contains("Other reply body"));
        assert!(text.contains("Has next page: false"));
        assert!(!text.contains("Next cursor:") && !text.contains("stale-comment-cursor"));
        let page = json!({"nodes":[{"id":"item-1","title":"One"}],"pageInfo":{"hasNextPage":false,"endCursor":"stale-list-cursor"}});
        let text = render_outcome("list_items", &json!({"type":"issue"}), &Outcome::ok(page));
        assert!(text.contains("Has next page: false"));
        assert!(!text.contains("Next cursor:") && !text.contains("stale-list-cursor"));
    }

    /// Invalid commit reports get a concrete format example without altering failure status.
    #[test]
    fn commit_format_error_shows_recovery_example() {
        let failed = Outcome {
            status: "blocked".into(),
            data: json!({"code":"INVALID_COMMIT_MESSAGE","message":"Result and Checks must be nonempty","retry":"Correct the reported condition."}),
        };
        let text = render_outcome("record_commits", &json!({"request_id":"req-1"}), &failed);
        assert!(text.contains("Result:\nWhat changed."));
        assert!(text.contains("Checks:\nWhat passed."));
        assert!(text.contains("request_id: req-1"));
    }

    /// Check that failure states retain retry guidance and zero/false fields do not become success.
    #[test]
    fn failure_and_success_are_distinct() {
        let request = json!({"request_id":"request-1"});
        let failed = Outcome {
            status: "outcome_unknown".into(),
            data: json!({"code":"LINEAR_UNAVAILABLE","message":"Maybe committed","retry":"Inspect, then retry same ID"}),
        };
        let text = render_outcome("edit_task", &request, &failed);
        assert!(text.contains("outcome_unknown: LINEAR_UNAVAILABLE"));
        assert!(text.contains("request-1"));
        assert!(text.contains("Inspect, then retry same ID"));
        let success = Outcome::ok(json!({"id":"item-1","count":0,"replayed":false}));
        let text = render_outcome("create_task", &request, &success);
        assert!(text.contains("ID: item-1"));
        assert!(text.contains("Replayed: false"), "{text}");
        assert!(!text.contains("count"));
    }

    /// Simulate a missing template after a confirmed mutation and retain its ID and outcome.
    #[test]
    fn presentation_failure_does_not_relabel_success() {
        assert!(render_template("missing", json!({})).is_err());
        let text = presentation_fallback(
            "create_task",
            &json!({"request_id":"request-1"}),
            &Outcome::ok(json!({"id":"item-1"})),
        );
        assert!(text.contains("ok"));
        assert!(text.contains("item-1"));
        assert!(text.contains("inspect context before retrying"));
        let malformed = render_outcome(
            "get_context",
            &json!({"id":"issue-1"}),
            &Outcome::ok(json!({})),
        );
        assert!(malformed.contains("get_context: ok"));
        assert!(malformed.contains("Presentation failed"));
    }

    /// Module reads keep the usable PR draft, notes and full source IDs without duplicating derived fields.
    #[test]
    fn module_report_keeps_draft_and_manual_prose_once() {
        let sha = "a".repeat(40);
        let description = "## Результат\nDerived summary\n\n## Результаты проверок\ncheck one\n\n## Manual notes\nKeep this human note.\n";
        let report = json!({"tasks_done":1,"tasks_total":1,"summary":"Derived summary","reported_checks":"check one","pr_draft":"## Summary\nDerived summary\n\n## Checks\ncheck one","notes":"Known risk","source_commits":[{"sha":sha,"subject":"feat: deliver","work_ids":["task-1"]}],"unfinished":[],"excluded":[]});
        let data = json!({"issue":{"id":"module-1","identifier":"MYT-1","title":"Module","url":"https://linear.app/module-1","description":description},"fields":{"result":"Derived summary","check_result":"check one"},"module_report":report.clone(),"children":[]});
        let text = render_outcome(
            "get_context",
            &json!({"type":"issue","id":"module-1"}),
            &Outcome::ok(data),
        );
        assert!(text.contains("## PR draft"), "{text}");
        assert!(text.contains("Known risk"));
        assert!(text.contains(&sha));
        assert!(text.contains("Keep this human note."));
        assert_eq!(text.matches("Derived summary").count(), 1);
        assert_eq!(text.matches("check one").count(), 1);
        let drift = json!({"issue":{"id":"module-1","description":"## Результат\nManual correction\n\n## Результаты проверок\ncheck one\n\n## Manual notes\nKeep this human note.\n"},"fields":{"result":"Derived summary","check_result":"check one"},"module_report":report,"children":[]});
        let text = render_outcome(
            "get_context",
            &json!({"type":"issue","id":"module-1"}),
            &Outcome::ok(drift),
        );
        assert!(text.contains("Manual correction"), "{text}");
        assert!(text.contains("Keep this human note."));
    }

    /// Overview pages retain the one reusable update draft, Epic totals and Module session link.
    #[test]
    fn overview_preserves_draft_counts_and_session() {
        let card = json!({"id":"module-1","identifier":"MYT-1","title":"Module","url":"https://linear.app/module-1","status":"In Progress","lead":"codex:lead","session_url":"https://example.com/session","report":{"tasks_done":1,"tasks_total":2,"summary":"Module result","reported_checks":"Build passed","unfinished":[]}});
        let full = json!({"project_id":"project-1","cursor":"cursor-1","project_update_draft":"## Draft body\nOne reusable update.","active_epics":[{"id":"epic-1","identifier":"MYT-2","title":"Epic","url":"https://linear.app/epic-1","status":"In Progress","tasks_done":1,"tasks_total":2,"modules":[card]}]});
        let text = render_outcome(
            "get_overview",
            &json!({"project_id":"project-1"}),
            &Outcome::ok(full),
        );
        for expected in [
            "Tasks Done: 1",
            "Tasks Total: 2",
            "Session URL: https://example.com/session",
            "## Draft body\nOne reusable update.",
        ] {
            assert!(text.contains(expected), "{text}");
        }
        assert_eq!(text.matches("One reusable update.").count(), 1);
        let delta = json!({"project_id":"project-1","cursor":"cursor-2","changes":[],"project_update_draft":"## Delta draft\nKeep me."});
        let text = render_outcome(
            "get_overview",
            &json!({"project_id":"project-1","cursor":"cursor-1"}),
            &Outcome::ok(delta),
        );
        assert!(text.contains("No changes since the supplied cursor."));
        assert!(text.contains("## Delta draft\nKeep me."));
    }

    /// Comment reads retain attribution and thread state; status checks identify a preview.
    #[test]
    fn activity_preview_and_fallback_keep_action_handles() {
        let comment = json!({"comment":{"id":"reply-1","url":"https://linear.app/reply-1","parent":{"id":"root-1"},"resolvedAt":null},"activity":{"kind":"question","actor":"codex:lead","role":"reviewer","session":"https://example.com/session","source_links":["https://example.com/source"],"parent_id":"root-1","resolved_at":null,"resolving_comment_id":null,"body":"Question"},"root":{"id":"root-1","url":"https://linear.app/root-1","body":"Root","resolvedAt":"2026-09-26T00:00:00Z","resolvingCommentId":"reply-2"},"replies":{"nodes":[],"pageInfo":{"hasNextPage":false}}});
        let text = render_outcome(
            "get_comment",
            &json!({"id":"reply-1"}),
            &Outcome::ok(comment),
        );
        for expected in [
            "Role: reviewer",
            "Session: https://example.com/session",
            "Source: https://example.com/source",
            "Parent ID: root-1",
            "Resolved: true",
            "Resolving comment ID: reply-2",
        ] {
            assert!(text.contains(expected), "{text}");
        }
        assert_eq!(text.matches("Parent ID: root-1").count(), 1);
        assert!(!text.contains("Resolved: false"));
        let preview = render_outcome(
            "move_status",
            &json!({"id":"issue-1","request_id":"req-1"}),
            &Outcome::ok(json!({"allowed":false,"conditions":["Need checks"],"status":"Done"})),
        );
        assert!(
            preview.contains("preview (check_only; no mutation)"),
            "{preview}"
        );
        assert!(preview.contains("Allowed: false"));
        assert!(!preview.contains("confirmed"));
        let fallback = presentation_fallback(
            "record_commits",
            &json!({"work_id":"task-1","request_id":"req-1"}),
            &Outcome {
                status: "blocked".into(),
                data: json!({"code":"INVALID_INPUT"}),
            },
        );
        assert!(fallback.contains("target_type: issue; target: task-1; request_id: req-1"));
        let issue = json!({"id":"issue-1","identifier":"MYT-1","title":"Task","url":"https://linear.app/issue-1","project":{"id":"project-1"},"team":{"id":"team-1"},"parent":{"id":"module-1"}});
        let ack = render_outcome(
            "create_task",
            &json!({"request_id":"req-1"}),
            &Outcome::ok(json!({"issue":issue})),
        );
        for handle in [
            "Project ID: project-1",
            "Team ID: team-1",
            "Parent ID: module-1",
            "Request ID: req-1",
        ] {
            assert!(ack.contains(handle), "{ack}");
        }
        let list = render_outcome(
            "list_items",
            &json!({"type":"issue"}),
            &Outcome::ok(json!({"nodes":[issue],"pageInfo":{"hasNextPage":false}})),
        );
        for handle in [
            "Project ID: project-1",
            "Team ID: team-1",
            "Parent ID: module-1",
        ] {
            assert!(list.contains(handle), "{list}");
        }
    }
}
