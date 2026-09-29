//! User artifact operations: `upload_file`, `list_files`, `get_file`. Attachments created
//! here carry `metadata.artifact`, a namespace distinct from the canonical `metadata.workflow`
//! state attachment in `records.rs`; neither reads nor writes the other's record.
use super::Gateway;
use crate::{
    linear::FILE_SIZE_CAP,
    model::{Fault, Result, require, text},
    records::child_id,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;

/// Map a filename extension to a content type; unknown or missing extensions are opaque bytes.
/// Extend this list when a genuinely new artifact type is needed.
fn content_type_for(filename: &str) -> &'static str {
    match filename
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "pdf" => "application/pdf",
        "json" => "application/json",
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}
/// The comparable intent of one artifact upload: same inputs must reproduce the same record.
fn intent(filename: &str, content_type: &str, size: u64, digest: &str, a: &Value) -> Value {
    json!({
        "filename":filename,"content_type":content_type,"size_bytes":size,"sha256":digest,
        "title":a.get("title"),"note":a.get("note"),
    })
}
/// Publish `bytes` at `destination` without ever overwriting different existing content.
/// Writes a sibling temporary file, then links it into place: `link(2)`/`CreateHardLink`
/// fail closed when the destination already exists, so no exists-check-then-rename race
/// can replace another file. Returns whether an identical file already occupied the spot.
fn publish(destination: &str, bytes: &[u8]) -> Result<bool> {
    let destination = Path::new(destination);
    require(
        destination.is_absolute(),
        "INVALID_INPUT",
        "destination must be an absolute path",
    )?;
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| Fault::new("INVALID_INPUT", "destination needs a parent directory"))?;
    let name = destination
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Fault::new("INVALID_INPUT", "destination needs a file name"))?;
    let tmp = parent.join(format!(".{name}.part-{}", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, bytes)
        .map_err(|_| Fault::new("FILE_UNREADABLE", "Could not write the temporary download"))?;
    let linked = std::fs::hard_link(&tmp, destination);
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => match std::fs::read(destination)
        {
            Ok(existing) if existing == bytes => Ok(true),
            Ok(_) => Err(Fault::new(
                "FILE_EXISTS",
                "A different file already exists at this destination",
            )),
            Err(_) => Err(Fault::new(
                "FILE_UNREADABLE",
                "Could not read the existing destination file",
            )),
        },
        Err(_) => Err(Fault::new(
            "FILE_UNREADABLE",
            "Could not publish the downloaded file",
        )),
    }
}

impl Gateway {
    /// Attach one local file to a work item as a durable native artifact. A retry with the
    /// deterministic per-request attachment ID and identical intent (filename, content type,
    /// size, digest, title, note) replays the existing record; changed intent conflicts.
    pub(super) async fn upload_file(&self, a: &Value) -> Result<Value> {
        let work_id = self.resolve("issue", text(a, "work_id")?).await?;
        let work = self.store.work(&work_id).await?;
        let path = text(a, "path")?;
        let source = Path::new(path);
        require(
            source.is_absolute(),
            "INVALID_INPUT",
            "path must be an absolute path",
        )?;
        let size = std::fs::metadata(source)
            .map_err(|_| Fault::new("FILE_UNREADABLE", "Local file is not readable"))?
            .len();
        require(
            size > 0 && size <= FILE_SIZE_CAP,
            "INVALID_INPUT",
            format!("File size must be 1..={FILE_SIZE_CAP} bytes"),
        )?;
        let bytes = std::fs::read(source)
            .map_err(|_| Fault::new("FILE_UNREADABLE", "Local file is not readable"))?;
        let digest = format!("{:x}", Sha256::digest(&bytes));
        let filename = source
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| Fault::new("INVALID_INPUT", "path needs a file name"))?;
        let content_type = content_type_for(filename);
        let request_id = text(a, "request_id")?;
        let aid = child_id(&work_id, &format!("artifact:{request_id}"));
        let requested = intent(filename, content_type, size, &digest, a);
        if let Some(existing) = self.store.optional("QArtifact", "attachment", &aid).await? {
            let stored = existing["metadata"]["artifact"].clone();
            require(
                stored.is_object(),
                "STATE_INVALID",
                "Attachment ID is already used by a non-artifact record",
            )?;
            require(
                stored == requested,
                "REQUEST_CONFLICT",
                "Upload request_id already names different file intent",
            )?;
            return Ok(json!({
                "id":existing["id"],"title":existing["title"],"url":existing["url"],
                "work_id":work_id,"work_url":work.native["url"],
                "file_name":filename,"content_type":content_type,"size_bytes":size,
                "replayed":true,
            }));
        }
        let upload_file = self
            .store
            .linear
            .reserve_upload(content_type, filename, size)
            .await?;
        self.store.linear.put_upload(&upload_file, bytes).await?;
        let asset_url = upload_file["assetUrl"]
            .as_str()
            .ok_or_else(|| {
                Fault::new(
                    "RECORD_MISSING",
                    "Upload reservation is missing its asset URL",
                )
            })?
            .to_owned();
        let title = a["title"].as_str().unwrap_or(filename);
        let attachment = self
            .store
            .linear
            .call(
                "MCreateArtifact",
                json!({"input":{
                    "id":aid,"issueId":work_id,"title":title,"url":asset_url,
                    "metadata":{"artifact":requested},
                }}),
            )
            .await?["attachmentCreate"]["attachment"]
            .clone();
        Ok(json!({
            "id":attachment["id"],"title":attachment["title"],"url":attachment["url"],
            "work_id":work_id,"work_url":work.native["url"],
            "file_name":filename,"content_type":content_type,"size_bytes":size,
            "replayed":false,
        }))
    }
    /// List this work item's user artifacts, excluding internal workflow-state attachments.
    pub(super) async fn list_files(&self, a: &Value) -> Result<Value> {
        let work_id = self.resolve("issue", text(a, "work_id")?).await?;
        let work = self.store.work(&work_id).await?;
        let nodes: Vec<Value> = self
            .store
            .linear
            .attachments(&work_id)
            .await?
            .into_iter()
            .filter(|attachment| attachment["metadata"]["artifact"].is_object())
            .map(|attachment| {
                let artifact = &attachment["metadata"]["artifact"];
                json!({
                    "id":attachment["id"],"title":attachment["title"],"url":attachment["url"],
                    "work_id":work_id,"work_url":work.native["url"],
                    "file_name":artifact["filename"],"content_type":artifact["content_type"],
                    "size_bytes":artifact["size_bytes"],
                })
            })
            .collect();
        Ok(json!({"nodes":nodes,"pageInfo":{"hasNextPage":false,"endCursor":Value::Null}}))
    }
    /// Download one artifact, verifying it is a user artifact and its content against the
    /// digest recorded at upload time, then publish it without overwriting another file.
    pub(super) async fn get_file(&self, a: &Value) -> Result<Value> {
        let id = text(a, "id")?;
        let attachment = self
            .store
            .optional("QArtifact", "attachment", id)
            .await?
            .ok_or_else(|| Fault::new("RECORD_MISSING", "Artifact not found"))?;
        let artifact = attachment["metadata"]["artifact"].clone();
        require(
            artifact.is_object(),
            "RECORD_MISSING",
            "Attachment is not a user artifact",
        )?;
        let work_id = text(&attachment["issue"], "id")?.to_owned();
        let work = self.store.work(&work_id).await?;
        let url = text(&attachment, "url")?;
        let (bytes, digest) = self.store.linear.get_asset(url).await?;
        require(
            artifact["sha256"].as_str() == Some(digest.as_str()),
            "INCOMPLETE_DATA",
            "Downloaded content does not match its recorded digest",
        )?;
        let destination = text(a, "destination")?;
        let replayed = publish(destination, &bytes)?;
        Ok(json!({
            "id":attachment["id"],"title":attachment["title"],"url":attachment["url"],
            "work_id":work_id,"work_url":work.native["url"],
            "file_name":artifact["filename"],"content_type":artifact["content_type"],
            "size_bytes":artifact["size_bytes"],"path":destination,"replayed":replayed,
        }))
    }
}
