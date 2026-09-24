//! Shared wire results, authenticated identities, and signed workflow records.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The server's classified failure; messages never include credentials or raw API bodies.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{code}: {message}")]
pub struct Fault {
    /// Stable machine-readable rejection code.
    pub code: String,
    /// Safe explanation of the failed condition.
    pub message: String,
    /// True when an external write may already have happened.
    pub uncertain: bool,
}

impl Fault {
    /// Construct a known rejection with no ambiguous write.
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            uncertain: false,
        }
    }

    /// Mark an error as an uncertain external write outcome.
    pub fn uncertain(mut self) -> Self {
        self.uncertain = true;
        self
    }
}

/// A fallible gateway operation; failures retain their stable code.
pub type Result<T> = std::result::Result<T, Fault>;

/// Authenticated role, resolved from protected server configuration.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Human owner authority.
    Owner,
    /// Workflow orchestrator authority without independent review authority.
    Root,
    /// Persistent module implementation responsibility.
    Lead,
    /// A bounded implementation helper.
    Helper,
    /// Planning responsibility without implementation acceptance.
    Decomposer,
    /// Independent review responsibility.
    Reviewer,
    /// Composition and integration responsibility.
    Integrator,
    /// Read-only access.
    Observer,
}

impl Role {
    /// Return the stable role name used in the tool catalogue and records.
    pub fn name(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Root => "root",
            Self::Lead => "lead",
            Self::Helper => "helper",
            Self::Decomposer => "decomposer",
            Self::Reviewer => "reviewer",
            Self::Integrator => "integrator",
            Self::Observer => "observer",
        }
    }
    /// Whether this role can coordinate product-wide work.
    pub fn controls(self) -> bool {
        matches!(self, Self::Owner | Self::Root)
    }
}

/// Scope and generation fixed by a protected MCP binding, never by tool arguments.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Principal {
    /// Stable logical actor identity, distinct from a Linear user UUID.
    pub id: String,
    /// Authority assigned to this binding.
    pub role: Role,
    /// Allowed product UUIDs; empty is allowed only for the owner bootstrap binding.
    #[serde(default)]
    pub products: Vec<String>,
    /// Optional assigned record; required for non-control mutating roles.
    pub assignment_id: Option<String>,
    /// Assignment generation fixed at provisioning.
    pub generation: Option<u64>,
    /// Product policy epoch fixed at provisioning; defaults to the initial epoch.
    #[serde(default = "one")]
    pub epoch: u64,
}

/// Return the initial policy epoch for omitted configuration values.
fn one() -> u64 {
    1
}

/// Machine-readable result returned as MCP structured content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    /// Read, committed mutation, rejection, or explicitly uncertain outcome.
    pub status: String,
    /// Original idempotency key when a mutation is involved.
    pub operation_key: Option<String>,
    /// Operation-specific data validated at each handler boundary.
    pub data: Value,
    /// Current expected tokens for the next request.
    pub version: Value,
    /// Advisory next actions; each call still revalidates its conditions.
    pub available_actions: Vec<Value>,
    /// Structured reasons a transition cannot proceed.
    pub violations: Vec<Value>,
    /// True when returned content is a bounded subset.
    pub incomplete: bool,
    /// Signed continuation bound to the actor and content hash.
    pub continuation: Option<String>,
    /// UTC time at which this response was assembled.
    pub observed_at: String,
}

impl Outcome {
    /// Build a complete successful read with empty advisory fields.
    pub fn ok(data: Value) -> Self {
        Self {
            status: "ok".into(),
            operation_key: None,
            data,
            version: json!({}),
            available_actions: vec![],
            violations: vec![],
            incomplete: false,
            continuation: None,
            observed_at: now(),
        }
    }
    /// Represent an error without losing its code or uncertain-outcome semantics.
    pub fn failure(fault: Fault, key: Option<String>) -> Self {
        let mut value = Self::ok(json!({}));
        value.status = if fault.uncertain {
            "outcome_unknown"
        } else if matches!(
            fault.code.as_str(),
            "LINEAR_UNAVAILABLE" | "LINEAR_TOKEN_MISSING" | "RATE_LIMITED"
        ) {
            "unavailable"
        } else {
            "blocked"
        }
        .into();
        value.operation_key = key;
        value.violations.push(json!({"code":fault.code,"explanation":fault.message,"retry_strategy":if fault.uncertain {"Inspect the saved operation before attempting another write."} else {"Correct the condition and refresh context."}}));
        value
    }
}

/// A signed envelope for one workflow fact; mutable records increment revision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Record {
    /// Wire format revision.
    pub schema_version: u32,
    /// Closed workflow fact kind, validated before use.
    pub record_kind: String,
    /// Preallocated UUID shared with its Linear attachment.
    pub record_id: String,
    /// Product control Issue UUID.
    pub product_id: String,
    /// Owning work Issue UUID.
    pub work_id: String,
    /// Monotonic revision of a mutable fact, starting at one.
    pub revision: u64,
    /// UTC creation/update observation time.
    pub created_at: String,
    /// Authenticated actor attribution including role and generation.
    pub actor: Value,
    /// Structured workflow fact, never an executable command from a document.
    pub payload: Value,
    /// SHA-256 of canonical JSON payload.
    pub payload_sha256: String,
    /// HMAC authentication tag covering all envelope fields except this tag.
    pub signature: Value,
}

/// Current UTC timestamp used in evidence and result envelopes.
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Extract a required nonempty string from an already shape-validated object.
pub fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| Fault::new("INVALID_INPUT", format!("{key} must be a nonempty string")))
}

/// Extract an array as a borrowed slice, using an empty slice for an absent optional field.
pub fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// Require a business condition or return a stable, safe rejection.
pub fn require(condition: bool, code: &str, reason: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Fault::new(code, reason))
    }
}
