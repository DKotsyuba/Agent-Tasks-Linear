//! Protected deployment configuration and per-principal MCP credentials.

use crate::model::{Fault, Principal, Result, Role, require};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    net::SocketAddr,
    path::{Path, PathBuf},
};
use uuid::Uuid;

/// One private bearer credential and the authority fixed to it at startup.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    /// Non-secret endpoint suffix under `/mcp/`.
    pub name: String,
    /// Random bearer token, never returned by an MCP tool or logs.
    pub token: String,
    /// Authenticated principal assigned to this credential.
    pub principal: Principal,
}
/// Deployment secrets and transport configuration; no workflow state lives here.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Fixed loopback address of the one active gateway.
    pub listen: SocketAddr,
    /// Random HMAC key retained across restarts so Linear facts remain verifiable.
    pub signing_key: String,
    /// Explicitly provisioned role bindings; changing them requires gateway restart.
    pub bindings: Vec<Binding>,
}
impl Config {
    /// Read a private config file; reject permissive Unix modes and invalid binding identities.
    pub fn load(path: &Path) -> Result<Self> {
        let meta = fs::metadata(path).map_err(|_| {
            Fault::new(
                "CONFIG_MISSING",
                "Run init-config before starting the gateway",
            )
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            require(
                meta.permissions().mode() & 0o077 == 0,
                "CONFIG_INVALID",
                "Configuration containing secrets must have mode 0600",
            )?;
        }
        let config: Self = toml::from_str(
            &fs::read_to_string(path)
                .map_err(|_| Fault::new("CONFIG_INVALID", "Cannot read configuration"))?,
        )
        .map_err(|_| Fault::new("CONFIG_INVALID", "Invalid configuration format"))?;
        config.validate()?;
        Ok(config)
    }
    /// Reject public listeners, weak secrets, duplicate identities and malformed scopes.
    pub fn validate(&self) -> Result<()> {
        require(
            self.listen.ip().is_loopback() && self.listen.port() != 0,
            "CONFIG_INVALID",
            "Gateway must listen on a fixed loopback port",
        )?;
        require(
            self.signing_key.len() >= 32,
            "CONFIG_INVALID",
            "Signing key is too short",
        )?;
        let mut names = BTreeSet::new();
        let mut tokens = BTreeSet::new();
        let mut actors = BTreeSet::new();
        for b in &self.bindings {
            require(
                !b.name.is_empty()
                    && b.name
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
                "CONFIG_INVALID",
                "Binding name must contain ASCII letters, digits, underscore or dash",
            )?;
            require(
                names.insert(&b.name) && tokens.insert(&b.token) && actors.insert(&b.principal.id),
                "CONFIG_INVALID",
                "Binding names, credentials and principal IDs must be distinct",
            )?;
            require(
                b.token.len() >= 32 && !b.principal.id.is_empty(),
                "CONFIG_INVALID",
                "Binding credential or principal is invalid",
            )?;
            require(
                !b.principal.products.is_empty() || b.principal.role == Role::Owner,
                "CONFIG_INVALID",
                "Only owner bootstrap may omit product scope",
            )?;
            for id in &b.principal.products {
                Uuid::parse_str(id).map_err(|_| {
                    Fault::new("CONFIG_INVALID", "Product scope must contain UUIDs")
                })?;
            }
        }
        Ok(())
    }
    /// Generate one owner bootstrap binding and signing secret without printing either secret.
    pub fn initialize(path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|_| {
                Fault::new("CONFIG_INVALID", "Cannot create configuration directory")
            })?;
        }
        let config = Self {
            listen: "127.0.0.1:8777".parse().unwrap(),
            signing_key: secret(),
            bindings: vec![Binding {
                name: "owner".into(),
                token: secret(),
                principal: Principal {
                    id: "owner".into(),
                    role: Role::Owner,
                    products: vec![],
                    assignment_id: None,
                    generation: None,
                    epoch: 1,
                },
            }],
        };
        config.write_new(path)
    }
    /// Write a new protected configuration exclusively; existing credentials are never overwritten.
    pub fn write_new(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path).map_err(|_| {
            Fault::new(
                "CONFIG_EXISTS",
                "Destination already exists or cannot be created",
            )
        })?;
        file.write_all(toml::to_string_pretty(self).unwrap().as_bytes())
            .map_err(|_| Fault::new("CONFIG_INVALID", "Cannot write configuration"))?;
        file.sync_all()
            .map_err(|_| Fault::new("CONFIG_INVALID", "Cannot sync configuration"))
    }
    /// Resolve a named binding without returning credentials in error text.
    pub fn binding(&self, name: &str) -> Result<&Binding> {
        self.bindings
            .iter()
            .find(|b| b.name == name)
            .ok_or_else(|| Fault::new("CONFIG_INVALID", "Unknown binding name"))
    }

    /// Add a private role credential with an atomic file replacement; the gateway must restart to load it.
    pub fn add_binding(path: &Path, name: String, principal: Principal) -> Result<()> {
        let mut config = Self::load(path)?;
        let before = fs::read(path)
            .map_err(|_| Fault::new("CONFIG_INVALID", "Cannot read configuration"))?;
        require(
            principal.role.controls()
                || principal.role == Role::Observer
                || (principal.assignment_id.is_some()
                    && principal.generation.is_some_and(|g| g > 0)),
            "CONFIG_INVALID",
            "Worker bindings require an assignment ID and generation",
        )?;
        if let Some(id) = &principal.assignment_id {
            Uuid::parse_str(id)
                .map_err(|_| Fault::new("CONFIG_INVALID", "Assignment ID must be a UUID"))?;
        }
        config.bindings.push(Binding {
            name,
            token: secret(),
            principal,
        });
        config.validate()?;
        let temporary = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
        config.write_new(&temporary)?;
        if fs::read(path).ok().as_deref() != Some(before.as_slice()) {
            let _ = fs::remove_file(&temporary);
            return Err(Fault::new(
                "CONFIG_CHANGED",
                "Configuration changed concurrently; retry after inspecting it",
            ));
        }
        fs::rename(&temporary, path)
            .map_err(|_| Fault::new("CONFIG_INVALID", "Cannot atomically replace configuration"))
    }
}
/// Default secret file location; the environment override is a local administrative input.
pub fn default_path() -> PathBuf {
    std::env::var_os("ATL_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                .join(".config/agent-tasks-linear/config.toml")
        })
}
/// Generate a 64-hex-character credential from two OS-random UUIDv4 values.
pub fn secret() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}
