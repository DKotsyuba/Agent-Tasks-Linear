//! Strict, embedded schemas for the public tools.
use crate::model::{Fault, Kind, Result};
use serde_json::Value;
/// Compiled request validators and MCP discovery descriptions.
pub struct Catalog {
    /// Tools in their stable discovery order.
    pub tools: Vec<Value>,
    /// Input validators at matching offsets.
    validators: Vec<jsonschema::Validator>,
}
impl Catalog {
    /// Validate the complete adopted fields through the same schema as a tool edit.
    pub fn validate_fields(&self, kind: Kind, fields: &Value) -> Result<()> {
        self.validate(
            &format!("edit_{}", kind.label().to_lowercase()),
            &serde_json::json!({
                "request_id":"00000000-0000-4000-8000-000000000001",
                "id":"00000000-0000-4000-8000-000000000002",
                "actor":"field-validation", "fields":fields
            }),
        )
    }
    /// Compile the self-contained catalogue with JSON format validation enabled.
    pub fn new() -> Result<Self> {
        let tools: Vec<Value> = serde_json::from_str(include_str!("../schemas/tools.json"))
            .map_err(|_| Fault::new("SCHEMA_UNSUPPORTED", "Invalid embedded catalogue"))?;
        let validators = tools
            .iter()
            .map(|t| {
                jsonschema::options()
                    .should_validate_formats(true)
                    .build(&t["inputSchema"])
                    .map_err(|e| Fault::new("SCHEMA_UNSUPPORTED", e.to_string()))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { tools, validators })
    }
    /// Reject unknown tools, unknown fields and invalid shapes before API access.
    pub fn validate(&self, name: &str, args: &Value) -> Result<()> {
        let i = self
            .tools
            .iter()
            .position(|t| t["name"] == name)
            .ok_or_else(|| Fault::new("UNKNOWN_TOOL", "Tool is not in the v2 catalogue"))?;
        self.validators[i]
            .validate(args)
            .map_err(|e| Fault::new("INVALID_INPUT", format!("{}: {}", e.instance_path, e)))
    }
}
