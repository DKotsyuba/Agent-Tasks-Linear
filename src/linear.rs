//! Bounded GraphQL transport: partial responses never imply a successful write.

use crate::model::{Fault, Result, require};
use reqwest::{Client, Url, header};
use serde_json::{Value, json};
use std::time::Duration;

/// The official endpoint; the API credential is never sent to document URLs.
pub const ENDPOINT: &str = "https://api.linear.app/graphql";
/// Static reviewed operations; callers select one by its declared name.
pub const OPERATIONS: &str = include_str!("graphql/operations.graphql");

/// An HTTP client with a protected authorization value and bounded responses.
#[derive(Clone)]
pub struct Linear {
    /// Pooled HTTP connection owner with redirects disabled.
    client: Client,
    /// Official endpoint, or an explicitly constructed loopback test service.
    endpoint: Url,
    /// Secret authentication header, absent until the user provides a token.
    authorization: Option<header::HeaderValue>,
}

impl Linear {
    /// Construct the production client, accepting a raw PAT or OAuth access token.
    pub fn new(token: Option<String>, oauth: bool) -> Result<Self> {
        Self::build(
            ENDPOINT,
            token.map(|t| if oauth { format!("Bearer {t}") } else { t }),
        )
    }
    /// Construct a mock client for an unauthenticated loopback HTTP fixture only.
    pub fn mock(endpoint: &str) -> Result<Self> {
        let url = Url::parse(endpoint)
            .map_err(|_| Fault::new("CONFIG_INVALID", "Invalid fixture URL"))?;
        require(
            url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")),
            "CONFIG_INVALID",
            "Fixtures must use loopback HTTP",
        )?;
        Self::build(endpoint, Some("fixture".into()))
    }
    /// Validate the secret without logging it and construct a nonredirecting client.
    fn build(endpoint: &str, token: Option<String>) -> Result<Self> {
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
                Err(_) => {
                    return Err(if write {
                        Fault::new(
                            "LINEAR_UNAVAILABLE",
                            "Linear write response was not received",
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
}
