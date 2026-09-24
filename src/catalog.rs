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
        "at_work_create" => "Record an epic, module, task or atomic work item under a parent.",
        "at_plan_publish" => {
            "Record a work goal and any available scope, criteria, inputs and dependencies."
        }
        "at_contract_confirm" => {
            "Record an attributed contract statement and any available preparation details."
        }
        "at_assign" => {
            "Record the principal responsible for the work and optional role, scope and execution context."
        }
        "at_execution_observe" => "Record a runtime observation and reported execution state.",
        "at_begin" => "Record that work has begun, with optional assignment and execution context.",
        "at_checkpoint" => {
            "Record a progress summary and any available artifacts or execution context."
        }
        "at_task_complete" => "Record a task result summary and its reported artifacts.",
        "at_complete" => {
            "Complete non-product work with a reported summary, artifacts and optional execution context."
        }
        "at_submit" => {
            "Record a work result for optional review, with its summary and reported artifacts."
        }
        "at_review_open" => "Open an optional review case and identify its reviewer.",
        "at_review_report" => {
            "Record a review summary and any available coverage, findings or supporting records."
        }
        "at_accept" => "Record acceptance of a reported work result with a reason.",
        "at_knowledge_save" => "Create or update a knowledge draft with its title and content.",
        "at_knowledge_publish" => {
            "Record publication of a knowledge note with optional references."
        }
        "at_question_ask" => {
            "Record a question about work with optional choices and blocked work references."
        }
        "at_owner_decide" => {
            "Record an owner decision with its rationale and available source details."
        }
        "at_change_propose" => "Record a proposed work change and its reason.",
        "at_candidate_register" => {
            "Record a candidate for an epic with any available composition and artifacts."
        }
        "at_integration_record" => {
            "Record an integration target and any available method or acceptance details."
        }
        "at_transfer" => {
            "Record a change in the principal responsible for the work and its reason."
        }
        "at_recovery_report" => {
            "Record a recovery report for work with any available status or preserved materials."
        }
        "at_recovery_confirm" => "Record confirmation that work recovery was reviewed.",
        "at_work_retire" => "Record a work disposition and reason.",
        "at_reconcile" => "Inspect or reconcile recorded workflow operations.",
        _ => "Unknown workflow intent.",
    }
}
