//! Bounded GraphQL transport: partial responses never imply a successful write.

use crate::model::{Fault, Result, require};
use reqwest::{Client, Url, header};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Duration;

/// The official endpoint; the API credential is never sent to document URLs.
pub const ENDPOINT: &str = "https://api.linear.app/graphql";
/// Static reviewed operations; callers select one by its declared name.
pub const OPERATIONS: &str = include_str!("graphql/operations.graphql");
/// The only host ever sent the Authorization header for a file transfer.
pub const ASSET_HOST: &str = "uploads.linear.app";
/// Product cap shared by upload and download transport: one file, one request.
pub const FILE_SIZE_CAP: u64 = 10_000_000;
/// Wall-clock bound for one signed upload or authenticated download.
const FILE_TIMEOUT: Duration = Duration::from_secs(60);

/// An HTTP client with a protected authorization value and bounded responses.
#[derive(Clone)]
pub struct Linear {
    /// Pooled HTTP connection owner with redirects disabled.
    client: Client,
    /// Official endpoint, or an explicitly constructed loopback test service.
    endpoint: Url,
    /// Secret authentication header, absent until the user provides a token.
    authorization: Option<header::HeaderValue>,
    /// Host allowed to receive the Authorization header for asset downloads;
    /// only a loopback fixture may differ from [`ASSET_HOST`].
    asset_host: String,
}

impl Linear {
    /// Construct the production client, accepting a raw PAT or OAuth access token.
    pub fn new(token: Option<String>, oauth: bool) -> Result<Self> {
        Self::build(
            ENDPOINT,
            token.map(|t| if oauth { format!("Bearer {t}") } else { t }),
            ASSET_HOST,
        )
    }
    /// Construct a mock client for an unauthenticated loopback HTTP fixture only.
    ///
    /// Asset transfers are also confined to this same loopback host, standing
    /// in for the production `uploads.linear.app` asset host.
    pub fn mock(endpoint: &str) -> Result<Self> {
        let url = Url::parse(endpoint)
            .map_err(|_| Fault::new("CONFIG_INVALID", "Invalid fixture URL"))?;
        require(
            url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")),
            "CONFIG_INVALID",
            "Fixtures must use loopback HTTP",
        )?;
        let asset_host = url.host_str().unwrap();
        Self::build(endpoint, Some("fixture".into()), asset_host)
    }
    /// Validate the secret without logging it and construct a nonredirecting client.
    fn build(endpoint: &str, token: Option<String>, asset_host: &str) -> Result<Self> {
        let authorization = token
            .map(|v| {
                let mut header = header::HeaderValue::from_str(&v)
                    .map_err(|_| Fault::new("CONFIG_INVALID", "Invalid API authorization value"))?;
                header.set_sensitive(true);
                Ok(header)
            })
            .transpose()?;
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| Fault::new("CONFIG_INVALID", "Cannot initialize HTTP transport"))?;
        Ok(Self {
            client,
            endpoint: Url::parse(endpoint).unwrap(),
            authorization,
            asset_host: asset_host.into(),
        })
    }
    /// Whether a credential was configured; this does not prove validity or permissions.
    pub fn configured(&self) -> bool {
        self.authorization.is_some()
    }
    /// Execute a declared operation; reads have two bounded retries, writes are never blindly retried.
    pub async fn call(&self, name: &str, variables: Value) -> Result<Value> {
        let write = name.starts_with('M');
        require(
            OPERATIONS.contains(&format!(
                "{} {name}",
                if write { "mutation" } else { "query" }
            )),
            "SCHEMA_UNSUPPORTED",
            "Undeclared GraphQL operation",
        )?;
        let auth = self.authorization.as_ref().ok_or_else(|| {
            Fault::new(
                "LINEAR_TOKEN_MISSING",
                "Set LINEAR_API_KEY or LINEAR_OAUTH_TOKEN before accessing Linear",
            )
        })?;
        for attempt in 0..3 {
            let response = self
                .client
                .post(self.endpoint.clone())
                .header(header::AUTHORIZATION, auth.clone())
                .json(&json!({"operationName":name,"query":OPERATIONS,"variables":variables}))
                .send()
                .await;
            let mut response = match response {
                Ok(r) => r,
                Err(_) if !write && attempt < 2 => {
                    tokio::time::sleep(Duration::from_millis(100 * (attempt + 1))).await;
                    continue;
                }
                Err(error) => {
                    let reason = if error.is_timeout() {
                        "timeout"
                    } else if error.is_connect() {
                        "connection failure"
                    } else {
                        "transport failure"
                    };
                    return Err(if write {
                        Fault::new(
                            "LINEAR_UNAVAILABLE",
                            format!("Linear {name} write response was not received ({reason})"),
                        )
                        .uncertain()
                    } else {
                        Fault::new("LINEAR_UNAVAILABLE", "Linear could not be reached")
                    });
                }
            };
            let status = response.status();
            if !write && (status.is_server_error() || status.as_u16() == 429) && attempt < 2 {
                let delay = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(1);
                if delay <= 5 {
                    tokio::time::sleep(Duration::from_secs(delay)).await;
                    continue;
                }
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| {
                if write {
                    Fault::new("LINEAR_PARTIAL_ERROR", "Incomplete Linear response").uncertain()
                } else {
                    Fault::new("LINEAR_UNAVAILABLE", "Incomplete Linear response")
                }
            })? {
                if bytes.len() + chunk.len() > 8 * 1024 * 1024 {
                    return Err(if write {
                        Fault::new(
                            "INCOMPLETE_DATA",
                            "Linear response exceeds the request budget",
                        )
                        .uncertain()
                    } else {
                        Fault::new(
                            "INCOMPLETE_DATA",
                            "Linear response exceeds the request budget",
                        )
                    });
                }
                bytes.extend_from_slice(&chunk);
            }
            let body: Value = serde_json::from_slice(&bytes).map_err(|_| {
                if write {
                    Fault::new("LINEAR_PARTIAL_ERROR", "Invalid Linear response").uncertain()
                } else {
                    Fault::new("LINEAR_UNAVAILABLE", "Invalid Linear response")
                }
            })?;
            let errors = body["errors"].as_array().filter(|e| !e.is_empty());
            let limited = status.as_u16() == 429
                || errors.is_some_and(|e| {
                    e.iter().any(|x| {
                        matches!(
                            x["extensions"]["code"].as_str(),
                            Some("RATELIMITED" | "RATE_LIMITED")
                        )
                    })
                });
            if !status.is_success() || errors.is_some() {
                let missing = errors.is_some_and(|items| {
                    items.iter().all(|e| {
                        matches!(
                            e["extensions"]["code"].as_str(),
                            Some("NOT_FOUND" | "ENTITY_NOT_FOUND")
                        ) || e["extensions"]["type"] == "EntityNotFound"
                            || (e["extensions"]["code"] == "INPUT_ERROR"
                                && e["extensions"]["type"] == "invalid input"
                                && e["message"].as_str().is_some_and(|message| {
                                    message.starts_with("Entity not found: ")
                                }))
                    })
                });
                let code = if missing && !write {
                    "RECORD_MISSING"
                } else if limited {
                    "RATE_LIMITED"
                } else if matches!(status.as_u16(), 401 | 403) {
                    "UNAUTHORIZED"
                } else {
                    "LINEAR_PARTIAL_ERROR"
                };
                let fault = Fault::new(
                    code,
                    format!("Linear rejected {name} (HTTP {})", status.as_u16()),
                );
                return Err(if write && !matches!(status.as_u16(), 401 | 403 | 429) {
                    fault.uncertain()
                } else {
                    fault
                });
            }
            let data = body
                .get("data")
                .filter(|d| d.is_object())
                .cloned()
                .ok_or_else(|| {
                    if write {
                        Fault::new("LINEAR_PARTIAL_ERROR", "Required response data is missing")
                            .uncertain()
                    } else {
                        Fault::new("RECORD_MISSING", "Required response data is missing")
                    }
                })?;
            if write
                && (data.as_object().unwrap().len() != 1
                    || data
                        .as_object()
                        .unwrap()
                        .values()
                        .any(|v| v["success"] != true))
            {
                return Err(
                    Fault::new("LINEAR_PARTIAL_ERROR", "Mutation did not confirm success")
                        .uncertain(),
                );
            }
            return Ok(data);
        }
        Err(Fault::new("LINEAR_UNAVAILABLE", "Read retries exhausted"))
    }
    /// Fetch a concrete native object or report missing data without interpreting null as success.
    pub async fn object(&self, query: &str, field: &str, id: &str) -> Result<Value> {
        self.call(query, json!({"id":id}))
            .await?
            .get(field)
            .filter(|v| v.is_object())
            .cloned()
            .ok_or_else(|| Fault::new("RECORD_MISSING", format!("Required {field} is missing")))
    }
    /// Read every attachment page for an Issue within a finite safety budget.
    pub async fn attachments(&self, id: &str) -> Result<Vec<Value>> {
        let mut out = vec![];
        let mut after = Value::Null;
        for _ in 0..100 {
            let body = self
                .call(
                    "QWorkAttachments",
                    json!({"id":id,"first":50,"after":after}),
                )
                .await?;
            let connection = &body["issue"]["attachments"];
            let nodes = connection["nodes"]
                .as_array()
                .ok_or_else(|| Fault::new("INCOMPLETE_DATA", "Attachment page is missing"))?;
            out.extend(nodes.iter().cloned());
            if connection["pageInfo"]["hasNextPage"] == false {
                return Ok(out);
            }
            let next = connection["pageInfo"]["endCursor"].clone();
            require(
                next.is_string() && next != after,
                "INCOMPLETE_DATA",
                "Attachment pagination did not advance",
            )?;
            after = next;
        }
        Err(Fault::new(
            "INCOMPLETE_DATA",
            "Attachment page budget exhausted",
        ))
    }
    /// Reserve a signed upload slot for one file, never a public asset.
    pub async fn reserve_upload(
        &self,
        content_type: &str,
        filename: &str,
        size: u64,
    ) -> Result<Value> {
        require(
            size > 0 && size <= FILE_SIZE_CAP,
            "INVALID_INPUT",
            format!("File size must be 1..={FILE_SIZE_CAP} bytes"),
        )?;
        let data = self
            .call(
                "MFileUpload",
                json!({"contentType":content_type,"filename":filename,"size":size,"makePublic":false}),
            )
            .await?;
        data["fileUpload"]["uploadFile"]
            .as_object()
            .cloned()
            .map(Value::Object)
            .ok_or_else(|| Fault::new("RECORD_MISSING", "Upload reservation is missing"))
    }
    /// Stream local bytes to a freshly reserved signed URL, sending only its native required headers.
    ///
    /// Never attaches the API Authorization header: the signed URL carries its
    /// own short-lived credential, which must not receive our long-lived token.
    pub async fn put_upload(&self, upload_file: &Value, bytes: Vec<u8>) -> Result<()> {
        let url = upload_file["uploadUrl"].as_str().ok_or_else(|| {
            Fault::new(
                "RECORD_MISSING",
                "Upload reservation is missing its signed URL",
            )
        })?;
        let mut request = self.client.put(url).timeout(FILE_TIMEOUT);
        for header in upload_file["headers"].as_array().into_iter().flatten() {
            if let (Some(key), Some(value)) = (header["key"].as_str(), header["value"].as_str()) {
                request = request.header(key, value);
            }
        }
        let response = request.body(bytes).send().await.map_err(|_| {
            Fault::new("LINEAR_UNAVAILABLE", "Signed upload was not received").uncertain()
        })?;
        require(
            response.status().is_success(),
            "LINEAR_PARTIAL_ERROR",
            format!(
                "Signed upload rejected the file (HTTP {})",
                response.status().as_u16()
            ),
        )
    }
    /// Stream one canonical asset with the existing Authorization header, bounded to the product cap.
    ///
    /// Refuses any host but the configured asset host so the credential is
    /// never sent to an attacker-controlled URL. Returns the bytes and their
    /// lowercase hex SHA-256 digest, computed while streaming.
    pub async fn get_asset(&self, asset_url: &str) -> Result<(Vec<u8>, String)> {
        let url = Url::parse(asset_url)
            .map_err(|_| Fault::new("INVALID_INPUT", "Malformed asset URL"))?;
        require(
            url.scheme() == "https" || self.asset_host != ASSET_HOST,
            "INVALID_INPUT",
            "Asset URL must use https",
        )?;
        require(
            url.host_str() == Some(self.asset_host.as_str()),
            "INVALID_INPUT",
            format!("Asset URL must be hosted on {}", self.asset_host),
        )?;
        let auth = self.authorization.as_ref().ok_or_else(|| {
            Fault::new(
                "LINEAR_TOKEN_MISSING",
                "Set LINEAR_API_KEY or LINEAR_OAUTH_TOKEN before accessing Linear",
            )
        })?;
        let mut response = self
            .client
            .get(url)
            .header(header::AUTHORIZATION, auth.clone())
            .timeout(FILE_TIMEOUT)
            .send()
            .await
            .map_err(|_| Fault::new("LINEAR_UNAVAILABLE", "Asset download was not received"))?;
        require(
            response.status().is_success(),
            "LINEAR_UNAVAILABLE",
            format!(
                "Asset download failed (HTTP {})",
                response.status().as_u16()
            ),
        )?;
        let mut bytes = Vec::new();
        let mut digest = Sha256::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| Fault::new("LINEAR_UNAVAILABLE", "Incomplete asset download"))?
        {
            require(
                bytes.len() as u64 + chunk.len() as u64 <= FILE_SIZE_CAP,
                "INCOMPLETE_DATA",
                format!("Asset exceeds the {FILE_SIZE_CAP}-byte product cap"),
            )?;
            digest.update(&chunk);
            bytes.extend_from_slice(&chunk);
        }
        Ok((bytes, format!("{:x}", digest.finalize())))
    }
}
