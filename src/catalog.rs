//! Versioned input schemas and role-filtered tool discovery.

use crate::model::{Fault, Result, Role};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// Compiled catalogue shared by all authenticated connections.
pub struct Catalog {
    /// Declarative schemas and role allowlists loaded from the embedded file.
    pub tools: Vec<Value>,
    /// Strict input validators in matching tool order.
    validators: Vec<jsonschema::Validator>,
}

impl Catalog {
    /// Compile all local schemas; remote schema retrieval is disabled by dependencies.
    pub fn new() -> Result<Self> {
        let mut source: Value = serde_json::from_str(include_str!("../schemas/tools.json"))
            .map_err(|_| Fault::new("SCHEMA_UNSUPPORTED", "Embedded catalogue is invalid JSON"))?;
        let defs = source["definitions"].clone();
        let mut tools = source["tools"].as_array_mut().unwrap().clone();
        let mut validators = vec![];
        for tool in &mut tools {
            let mut required = BTreeSet::new();
            referenced(&tool["inputSchema"], &mut required);
            loop {
                let previous = required.len();
                for name in required.clone() {
                    referenced(&defs[&name], &mut required);
                }
                if previous == required.len() {
                    break;
                }
            }
            if !required.is_empty() {
                tool["inputSchema"]["$defs"] = Value::Object(
                    required
                        .into_iter()
                        .map(|name| (name.clone(), defs[&name].clone()))
                        .collect(),
                );
            }
            validators.push(
                jsonschema::options()
                    .should_validate_formats(true)
                    .build(&tool["inputSchema"])
                    .map_err(|e| Fault::new("SCHEMA_UNSUPPORTED", e.to_string()))?,
            );
        }
        Ok(Self { tools, validators })
    }
    /// Resolve a tool, enforce role access, then validate every input field and format.
    pub fn validate(&self, name: &str, role: Role, args: &Value) -> Result<bool> {
        let i = self
            .tools
            .iter()
            .position(|t| t["name"] == name)
            .ok_or_else(|| Fault::new("UNKNOWN_TOOL", "Unknown workflow tool"))?;
        if !self.tools[i]["roles"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == role.name())
        {
            return Err(Fault::new(
                "UNAUTHORIZED",
                "This binding cannot call this tool",
            ));
        }
        if let Some(error) = self.validators[i].iter_errors(args).next() {
            return Err(Fault::new(
                "INVALID_INPUT",
                format!("Invalid input at {}", error.instance_path),
            ));
        }
        Ok(self.tools[i]["mode"] == "write"
            && !(name == "at_reconcile" && args["mode"] == "inspect"))
    }
    /// Produce MCP tool definitions without tools outside the authenticated role.
    pub fn visible(&self, role: Role) -> Vec<Value> {
        self.tools.iter().filter(|t|t["roles"].as_array().unwrap().iter().any(|r|r==role.name())).map(|t|json!({"name":t["name"],"description":description(t["name"].as_str().unwrap()),"inputSchema":t["inputSchema"],"annotations":{"readOnlyHint":t["mode"]=="read","destructiveHint":false,"idempotentHint":true,"openWorldHint":true}})).collect()
    }
}

/// Collect local definition references so each tool exposes only relevant input types.
fn referenced(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            if let Some(name) = map
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|v| v.strip_prefix("#/$defs/"))
            {
                out.insert(name.into());
            }
            for value in map.values() {
                referenced(value, out)
            }
        }
        Value::Array(items) => {
            for item in items {
                referenced(item, out)
            }
        }
        _ => {}
    }
}

/// Return a concise English intent description; content in Linear never grants permissions.
fn description(name: &str) -> &'static str {
    match name {
        "at_resume" => {
            "Read current work, assignments, blockers and suggested next actions without claiming work."
        }
        "at_context" => {
            "Read one scoped work section or exact signed record/publication with bounded continuation."
        }
        "at_search" => "Search permitted work and published knowledge in Linear.",
        "at_work_create" => {
            "Create an epic, module, task or atomic work under a valid immutable parent."
        }
        "at_plan_publish" => {
            "Publish immutable obligations and pinned inputs; plan changes require an approved proposal."
        }
        "at_contract_confirm" => {
            "Record an attributed contract agreement and observed preparation for an exact version."
        }
        "at_assign" => {
            "Assign persistent responsibility without replacing an existing healthy assignee."
        }
        "at_execution_observe" => {
            "Record an external runtime observation; success does not accept work."
        }
        "at_begin" => {
            "Begin work only after plan, scope, assignment, preparation and writer gates pass."
        }
        "at_checkpoint" => {
            "Save progress, immutable evidence and author responses without closing review findings."
        }
        "at_task_complete" => {
            "Complete a task locally with required evidence and published knowledge; does not accept its module."
        }
        "at_submit" => "Freeze a module, atomic or epic result for independent review.",
        "at_review_open" => {
            "Assign an independent reviewer while preserving the existing review case and findings."
        }
        "at_review_report" => {
            "Record reviewer coverage, evidence and verified finding outcomes for the current submission."
        }
        "at_accept" => {
            "Accept the exact current reviewed result after all required children and evidence pass."
        }
        "at_knowledge_save" => "Create or update a native draft using its expected content hash.",
        "at_knowledge_publish" => {
            "Publish a separate immutable knowledge snapshot while preserving older pinned versions."
        }
        "at_question_ask" => {
            "Save an explicit owner question and blocked work; elapsed time is not approval."
        }
        "at_owner_decide" => {
            "Record an authenticated or policy-attributed owner decision with an exact source."
        }
        "at_change_propose" => {
            "Propose changes to current obligations without applying or approving them."
        }
        "at_candidate_register" => {
            "Record actual epic composition and compute completeness from exact component results."
        }
        "at_integration_record" => {
            "Record evidence of accepted output in its actual target without executing Git."
        }
        "at_transfer" => {
            "Transfer responsibility only after verified writer stop; require recovery under a new generation."
        }
        "at_recovery_report" => {
            "Report understanding of the current plan and preserved materials as the new assignee."
        }
        "at_recovery_confirm" => {
            "Confirm a new assignee's recovery; beginning work remains a separate guarded action."
        }
        "at_work_retire" => "Retire unaccepted work with an explicit reason and stopped writers.",
        "at_reconcile" => {
            "Inspect saved operations without writing, or safely reconcile recorded effects with exact IDs."
        }
        _ => "Unknown workflow intent.",
    }
}
