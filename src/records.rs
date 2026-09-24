//! Signed Linear attachments and request-local traversal of committed work.

use crate::{
    linear::Linear,
    model::{Fault, Principal, Record, Result, array, now, require, text},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

/// Canonical JSON rejects floating point numbers and uses sorted object keys.
pub fn canonical(value: &Value) -> Result<Vec<u8>> {
    match value {
        Value::Number(n) => require(
            n.is_i64() || n.is_u64(),
            "INVALID_INPUT",
            "Canonical records permit integer numbers only",
        )?,
        Value::Array(a) => {
            for v in a {
                canonical(v)?;
            }
        }
        Value::Object(m) => {
            for v in m.values() {
                canonical(v)?;
            }
        }
        _ => {}
    }
    serde_json::to_vec(value)
        .map_err(|_| Fault::new("INVALID_INPUT", "Cannot encode canonical JSON"))
}
/// SHA-256 of canonical structured data.
pub fn hash(value: &Value) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(canonical(value)?)))
}
/// SHA-256 of exact UTF-8 document content returned by Linear.
pub fn content_hash(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}

/// Protected signing material; never implements Debug or serializes its secret.
#[derive(Clone)]
pub struct Signer {
    /// Random secret of at least 32 bytes.
    key: Vec<u8>,
}
impl Signer {
    /// Validate secret entropy length; provisioning supplies cryptographically random bytes.
    pub fn new(key: impl AsRef<[u8]>) -> Result<Self> {
        require(
            key.as_ref().len() >= 32,
            "CONFIG_INVALID",
            "Signing key must contain at least 32 bytes",
        )?;
        Ok(Self {
            key: key.as_ref().to_vec(),
        })
    }
    /// Authenticate canonical data with HMAC-SHA256.
    pub fn tag(&self, value: &Value) -> Result<String> {
        let mut h = Hmac::<Sha256>::new_from_slice(&self.key).unwrap();
        h.update(&canonical(value)?);
        Ok(URL_SAFE_NO_PAD.encode(h.finalize().into_bytes()))
    }
    /// Verify a tag using the library's constant-time MAC check.
    pub fn verify_tag(&self, value: &Value, tag: &str) -> Result<()> {
        let bytes = URL_SAFE_NO_PAD
            .decode(tag)
            .map_err(|_| Fault::new("SNAPSHOT_TAMPERED", "Invalid signature encoding"))?;
        let mut h = Hmac::<Sha256>::new_from_slice(&self.key).unwrap();
        h.update(&canonical(value)?);
        h.verify_slice(&bytes)
            .map_err(|_| Fault::new("SNAPSHOT_TAMPERED", "Record signature does not match"))
    }
    /// Seal a record after setting the canonical payload hash and timestamp.
    pub fn seal(&self, record: &mut Record) -> Result<()> {
        record.payload_sha256 = hash(&record.payload)?;
        record.signature = Value::Null;
        let mut data = serde_json::to_value(&*record).unwrap();
        data.as_object_mut().unwrap().remove("signature");
        record.signature =
            json!({"kid":"deployment-v1","algorithm":"HMAC-SHA256","value":self.tag(&data)?});
        Ok(())
    }
    /// Verify scope-bound content and its schema revision before any field is trusted.
    pub fn verify(&self, record: &Record) -> Result<()> {
        require(
            record.schema_version == 1,
            "SCHEMA_UNSUPPORTED",
            "Unsupported record schema",
        )?;
        require(
            record.payload_sha256 == hash(&record.payload)?,
            "SNAPSHOT_TAMPERED",
            "Payload hash does not match",
        )?;
        require(
            record.signature["algorithm"] == "HMAC-SHA256"
                && record.signature["kid"] == "deployment-v1",
            "SNAPSHOT_TAMPERED",
            "Unsupported signing key or algorithm",
        )?;
        let mut data = serde_json::to_value(record).unwrap();
        data.as_object_mut().unwrap().remove("signature");
        self.verify_tag(&data, text(&record.signature, "value")?)
    }
    /// Create a signed immutable fact, using a caller-reserved UUID when supplied.
    pub fn record(
        &self,
        actor: &Principal,
        product: &str,
        work: &str,
        kind: &str,
        payload: Value,
        id: Option<String>,
    ) -> Result<Record> {
        let mut r = Record {
            schema_version: 1,
            record_kind: kind.into(),
            record_id: id.unwrap_or_else(|| Uuid::new_v4().to_string()),
            product_id: product.into(),
            work_id: work.into(),
            revision: 1,
            created_at: now(),
            actor: json!({"principal_id":actor.id,"role":actor.role,"generation":actor.generation}),
            payload,
            payload_sha256: String::new(),
            signature: Value::Null,
        };
        self.seal(&mut r)?;
        Ok(r)
    }
    /// Create a cursor authenticated against substitution; cursor content is not confidential.
    pub fn cursor(&self, value: &Value) -> Result<String> {
        Ok(format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(canonical(value)?),
            self.tag(value)?
        ))
    }
    /// Decode and authenticate a cursor before considering any of its fields.
    pub fn decode_cursor(&self, token: &str) -> Result<Value> {
        let (data, tag) = token
            .split_once('.')
            .ok_or_else(|| Fault::new("STALE_CONTEXT", "Invalid continuation"))?;
        let value: Value = serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(data)
                .map_err(|_| Fault::new("STALE_CONTEXT", "Invalid continuation"))?,
        )
        .map_err(|_| Fault::new("STALE_CONTEXT", "Invalid continuation"))?;
        self.verify_tag(&value, tag)?;
        Ok(value)
    }
}

/// One committed work's authoritative identity, head and current native fields.
#[derive(Clone)]
pub struct Work {
    /// Current Linear Issue fields; titles remain native.
    pub native: Value,
    /// Immutable parent and native container mapping.
    pub identity: Record,
    /// Mutable workflow pointers and direct child IDs.
    pub head: Record,
}
/// Request-local facts; discarded when the request completes, never persisted locally.
#[derive(Clone)]
pub struct Snapshot {
    /// Product control Issue UUID.
    pub product: String,
    /// Current product policy and native IDs.
    pub config: Record,
    /// Committed work tree indexed by native Issue UUID.
    pub works: BTreeMap<String, Work>,
    /// Signed facts indexed by preallocated record UUID.
    pub records: BTreeMap<String, Record>,
}
impl Snapshot {
    /// Latest exact record written by a committed recipe, including the initial head of newly created work.
    pub fn latest_committed(&self, id: &str) -> Option<&Value> {
        self.records
            .values()
            .filter(|r| r.record_kind == "operation_receipt" && r.payload["state"] == "committed")
            .flat_map(|r| array(&r.payload["plan"], "effects"))
            .filter_map(|effect| match effect["kind"].as_str() {
                Some("record") => Some(&effect["record"]),
                Some("new_work") => Some(&effect["head"]),
                _ => None,
            })
            .filter(|record| record["record_id"] == id)
            .max_by_key(|value| value["revision"].as_u64().unwrap_or(0))
    }
    /// Whether this exact record version belongs to the latest committed receipt for its identity.
    pub fn is_committed(&self, record: &Record) -> bool {
        self.latest_committed(&record.record_id)
            .is_some_and(|value| *value == serde_json::to_value(record).unwrap())
    }
    /// Require an existing committed work in this product.
    pub fn work(&self, id: &str) -> Result<&Work> {
        self.works
            .get(id)
            .ok_or_else(|| Fault::new("OUT_OF_SCOPE", "Work is not committed in this product"))
    }
    /// Require an existing signed record, optionally constraining its fact kind.
    pub fn record(&self, id: &str, kind: Option<&str>) -> Result<&Record> {
        let r = self
            .records
            .get(id)
            .ok_or_else(|| Fault::new("RECORD_MISSING", "Required record is missing"))?;
        require(
            kind.is_none_or(|k| k == r.record_kind),
            "INVALID_INPUT",
            "Referenced record has the wrong kind",
        )?;
        Ok(r)
    }
    /// Test immutable ancestry without relying on client scope strings.
    pub fn within(&self, id: &str, root: &str) -> bool {
        let mut current = id;
        let mut seen = BTreeSet::new();
        while seen.insert(current.to_owned()) {
            if current == root {
                return true;
            }
            let Some(work) = self.works.get(current) else {
                return false;
            };
            let Some(parent) = work.identity.payload["primary_parent_id"].as_str() else {
                return false;
            };
            current = parent;
        }
        false
    }
}

/// Reads and writes signed records using only the official Linear client.
#[derive(Clone)]
pub struct Store {
    /// Concrete HTTP adapter shared by admin and workflow operations.
    pub linear: Linear,
    /// Record authentication and cursor signing.
    pub signer: Signer,
}
impl Store {
    /// Serialize one envelope into flat attachment metadata, rejecting excessive records.
    pub fn attachment_input(&self, record: &Record, base_url: &str) -> Result<Value> {
        let body = serde_json::to_string(record).unwrap();
        require(
            body.len() <= 256 * 1024,
            "RECORD_TOO_LARGE",
            "Record exceeds the provisional 256 KiB safety cap",
        )?;
        Ok(
            json!({"id":record.record_id,"issueId":record.work_id,"title":format!("AT {} · r{}",record.record_kind,record.revision),"url":format!("{base_url}#agent-tasks-v1/{}/{}",record.record_kind,record.record_id),"metadata":{"at_schema":1,"at_kind":record.record_kind,"at_product_id":record.product_id,"at_work_id":record.work_id,"payload_json":body}}),
        )
    }
    /// Upsert once, then verify bounded read-back; older replicated versions cause read-only retries.
    pub async fn put(&self, record: &Record, base_url: &str) -> Result<()> {
        let input = self.attachment_input(record, base_url)?;
        let reply = self
            .linear
            .call("MUpsertRecord", json!({"input":input}))
            .await?;
        require(
            reply["attachmentCreate"]["attachment"]["id"] == record.record_id,
            "LINEAR_PARTIAL_ERROR",
            "Attachment ID changed during upsert",
        )
        .map_err(Fault::uncertain)?;
        self.verify(record).await
    }
    /// Verify an exact signed version with bounded read-only retries; never repeat an external write.
    pub async fn verify(&self, record: &Record) -> Result<()> {
        let mut observed = None;
        for delay in [0, 50, 100, 200, 400] {
            if delay > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
            let fetched = match self
                .linear
                .object("QAttachmentById", "attachment", &record.record_id)
                .await
            {
                Ok(value) => value,
                Err(error) if error.code == "RECORD_MISSING" => continue,
                Err(error) => return Err(error.uncertain()),
            };
            let saved = self.parse(&fetched, &record.product_id)?.ok_or_else(|| {
                Fault::new("SNAPSHOT_TAMPERED", "Missing managed record").uncertain()
            })?;
            if saved == *record {
                return Ok(());
            }
            observed = Some(saved.revision);
            if saved.revision >= record.revision {
                let actual = serde_json::to_value(&saved).unwrap();
                let expected = serde_json::to_value(record).unwrap();
                let fields = expected
                    .as_object()
                    .unwrap()
                    .iter()
                    .filter(|(key, value)| actual.get(*key) != Some(*value))
                    .map(|(key, _)| key.as_str())
                    .collect::<Vec<_>>();
                return Err(Fault::new(
                    "SNAPSHOT_TAMPERED",
                    format!(
                        "Record read-back differs at revision {} in {}",
                        saved.revision,
                        fields.join(", ")
                    ),
                )
                .uncertain());
            }
        }
        Err(Fault::new(
            "OPERATION_IN_DOUBT",
            format!(
                "Record read-back remained stale: expected revision {}, observed {:?}",
                record.revision, observed
            ),
        )
        .uncertain())
    }
    /// Parse only this product's metadata and reject corruption before returning the record.
    pub fn parse(&self, attachment: &Value, product: &str) -> Result<Option<Record>> {
        let meta = &attachment["metadata"];
        if meta["at_product_id"] != product {
            return Ok(None);
        }
        let r: Record = serde_json::from_str(text(meta, "payload_json")?).map_err(|_| {
            Fault::new(
                "SNAPSHOT_TAMPERED",
                "Managed attachment is not a valid envelope",
            )
        })?;
        self.signer.verify(&r)?;
        require(
            r.product_id == product
                && meta["at_work_id"] == r.work_id
                && meta["at_kind"] == r.record_kind
                && attachment["id"] == r.record_id,
            "SNAPSHOT_TAMPERED",
            "Attachment identity does not match its signature",
        )?;
        Ok(Some(r))
    }
    /// Load the bounded committed work tree; missing pages never become an empty success.
    pub async fn snapshot(&self, product: &str) -> Result<Snapshot> {
        let mut works = BTreeMap::new();
        let mut records = BTreeMap::new();
        let mut queue = vec![product.to_owned()];
        let mut config = None;
        // ponytail: request-local full tree, capped at 200 works; switch to exact dependency reads when API cost warrants it.
        while let Some(id) = queue.pop() {
            require(
                works.len() < 200,
                "INCOMPLETE_DATA",
                "Product exceeds the 200-work request budget",
            )?;
            require(
                !works.contains_key(&id),
                "STRUCTURE_DRIFT",
                "Work tree contains a repeated child or cycle",
            )?;
            let native = self.linear.object("QIssue", "issue", &id).await?;
            let attachments = self.linear.attachments(&id).await?;
            let mut identity = None;
            let mut head = None;
            for attachment in attachments {
                if let Some(record) = self.parse(&attachment, product)? {
                    require(
                        record.work_id == id,
                        "SNAPSHOT_TAMPERED",
                        "Record is attached to another work",
                    )?;
                    match record.record_kind.as_str() {
                        "identity" => {
                            require(identity.is_none(), "STRUCTURE_DRIFT", "Multiple identities")?;
                            identity = Some(record.clone())
                        }
                        "work_head" => {
                            require(head.is_none(), "STRUCTURE_DRIFT", "Multiple work heads")?;
                            head = Some(record.clone())
                        }
                        "product_config" if id == product => {
                            require(
                                config.is_none(),
                                "STRUCTURE_DRIFT",
                                "Multiple product configurations",
                            )?;
                            config = Some(record.clone())
                        }
                        _ => {}
                    };
                    require(
                        records.insert(record.record_id.clone(), record).is_none(),
                        "SNAPSHOT_TAMPERED",
                        "Duplicate record identity",
                    )?;
                }
            }
            let identity = identity.ok_or_else(|| {
                Fault::new("RECORD_MISSING", "Committed work identity is missing")
            })?;
            let head =
                head.ok_or_else(|| Fault::new("RECORD_MISSING", "Committed work head is missing"))?;
            queue.extend(
                array(&head.payload, "children")
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned),
            );
            works.insert(
                id,
                Work {
                    native,
                    identity,
                    head,
                },
            );
        }
        let config = config.ok_or_else(|| {
            Fault::new(
                "PRODUCT_NOT_INITIALIZED",
                "Run the administrative bootstrap before using this product",
            )
        })?;
        Ok(Snapshot {
            product: product.into(),
            config,
            works,
            records,
        })
    }
    /// Verify a pinned document and product membership against its signed snapshot hash.
    pub async fn document(&self, snapshot: &Snapshot, record: &Record) -> Result<Value> {
        let doc = self
            .linear
            .object(
                "QDocument",
                "document",
                text(&record.payload, "document_id")?,
            )
            .await?;
        require(
            doc["project"]["id"] == snapshot.config.payload["general_project_id"],
            "OUT_OF_SCOPE",
            "Document is outside the product knowledge project",
        )?;
        if let Some(expected) = record.payload["content_hash"].as_str() {
            require(
                content_hash(doc["content"].as_str().unwrap_or("")) == expected,
                "SNAPSHOT_TAMPERED",
                "Published document content changed",
            )?;
        }
        Ok(doc)
    }
}
