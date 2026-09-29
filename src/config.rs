//! Protected local transport configuration; workflow state lives in Linear.
use crate::model::{Fault, Result, require};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    net::SocketAddr,
    path::{Path, PathBuf},
};
/// One loopback listener and bearer credential shared by trusted agent clients.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Fixed loopback address; a second writer cannot bind the same port.
    pub listen: SocketAddr,
    /// Secret bearer token, never exposed in tools or logs.
    pub token: String,
}
impl Config {
    /// Read a private file and validate its listener and secret; old configurations must be replaced explicitly.
    pub fn load(path: &Path) -> Result<Self> {
        let meta = fs::metadata(path)
            .map_err(|_| Fault::new("CONFIG_MISSING", "Run init-config with a new config path"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            require(
                meta.permissions().mode() & 0o077 == 0,
                "CONFIG_INVALID",
                "Config must have mode 0600",
            )?;
        }
        let cfg: Self = toml::from_str(
            &fs::read_to_string(path)
                .map_err(|_| Fault::new("CONFIG_INVALID", "Cannot read config"))?,
        )
        .map_err(|_| {
            Fault::new(
                "CONFIG_INVALID",
                "Invalid config; v2 uses listen and token only",
            )
        })?;
        cfg.validate()?;
        Ok(cfg)
    }
    /// Reject public listeners, dynamic ports and weak/invalid bearer credentials.
    pub fn validate(&self) -> Result<()> {
        require(
            self.listen.ip().is_loopback() && self.listen.port() != 0,
            "CONFIG_INVALID",
            "Use a fixed loopback port",
        )?;
        require(
            self.token.len() >= 32
                && self
                    .token
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "CONFIG_INVALID",
            "Use a bearer credential of at least 32 safe characters",
        )
    }
    /// Create a fresh protected config without overwriting an existing credential.
    pub fn initialize(path: &Path) -> Result<()> {
        Self {
            listen: "127.0.0.1:8777".parse().unwrap(),
            token: format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            ),
        }
        .write_new(path)
    }
    /// Persist exclusively with Unix mode 0600; error if the path already exists.
    pub fn write_new(&self, path: &Path) -> Result<()> {
        self.validate()?;
        if let Some(p) = path.parent() {
            fs::create_dir_all(p)
                .map_err(|_| Fault::new("CONFIG_INVALID", "Cannot create config directory"))?;
        }
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts
            .open(path)
            .map_err(|_| Fault::new("CONFIG_EXISTS", "Config exists or cannot be created"))?;
        file.write_all(toml::to_string_pretty(self).unwrap().as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|_| Fault::new("CONFIG_INVALID", "Cannot save config"))
    }
}
/// Resolve ATL_CONFIG or the user's standard configuration location.
pub fn default_path() -> PathBuf {
    std::env::var_os("ATL_CONFIG")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                .join(".config/agent-tasks-linear/config.toml")
        })
}
