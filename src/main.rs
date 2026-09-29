//! Minimal CLI for the authenticated loopback writer and standard stdio bridge.
use agent_tasks_linear::{
    catalog::Catalog,
    config::{self, Config},
    gateway::Gateway,
    linear::Linear,
    model::{Fault, Result},
    render, server,
};
use clap::{Parser, Subcommand};
use serde_json::json;
use std::path::{Path, PathBuf};

/// Select a protected local configuration and one explicit service operation.
#[derive(Parser)]
#[command(version, about = "Basic native Linear workflow MCP")]
struct Cli {
    /// Private deployment configuration, defaulting to ATL_CONFIG.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Operation; no workflow actions are hidden in the CLI.
    #[command(subcommand)]
    command: Command,
}
/// Supported lifecycle operations with family-standard names and legacy aliases.
#[derive(Subcommand)]
enum Command {
    /// Run the protocol-only stdio bridge to the resident writer.
    Mcp,
    /// Legacy alias of mcp.
    Stdio,
    /// Generate a new private bearer config; never overwrites existing credentials.
    Init,
    /// Legacy alias of init.
    InitConfig,
    /// Run the single loopback MCP writer.
    Serve,
    /// Read-only local health report; never contacts Linear or the network
    /// unless --online is passed explicitly.
    Doctor {
        /// Emit one machine-readable JSON document.
        #[arg(long)]
        json: bool,
        /// Additionally verify the authenticated Linear viewer; needs credentials.
        #[arg(long)]
        online: bool,
    },
    /// Local configuration operations without migration or network access.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Install one verified single-binary bundle. Never restarts services.
    SelfInstall {
        /// Verified bundle directory holding the binary and its manifest.
        #[arg(long)]
        bundle: PathBuf,
        /// Absolute product home for immutable releases and the launcher.
        #[arg(long)]
        home: PathBuf,
        /// Absolute directory receiving the managed launcher.
        #[arg(long)]
        bin_dir: PathBuf,
        /// Explicitly adopt a known legacy plain executable at the launcher
        /// path: identity-checked, preserved byte-exactly as a backup.
        #[arg(long)]
        adopt_existing: bool,
    },
    /// Select a retained compatible installation.
    Releases {
        #[command(subcommand)]
        command: ReleaseCommand,
    },
}
/// Release selection subcommands.
#[derive(Subcommand)]
enum ReleaseCommand {
    /// Activate one retained version after integrity and state-profile checks.
    Use {
        /// Retained version, e.g. 0.5.0.
        version: String,
        /// Absolute product home.
        #[arg(long)]
        home: PathBuf,
        /// Absolute directory holding the managed launcher.
        #[arg(long)]
        bin_dir: PathBuf,
    },
}
/// Config subcommands.
#[derive(Subcommand)]
enum ConfigCommand {
    /// Validate the protected config file locally.
    Check,
}
/// Run the command, reserving stdout for MCP protocol when using stdio.
#[tokio::main]
async fn main() {
    if let Err(e) = run(Cli::parse()).await {
        eprintln!("{e}");
        std::process::exit(1)
    }
}
/// Load protected configuration and dispatch without printing API or bearer secrets.
#[allow(
    clippy::print_stdout,
    reason = "Explicit CLI branches only; the stdio branch writes protocol exclusively through the transport"
)]
async fn run(cli: Cli) -> Result<()> {
    let path = cli.config.unwrap_or_else(config::default_path);
    match cli.command {
        Command::InitConfig | Command::Init => {
            Config::initialize(&path)?;
            println!("Created private configuration: {}", path.display());
            Ok(())
        }
        Command::Mcp | Command::Stdio => server::stdio(&Config::load(&path)?).await,
        Command::Serve => {
            let cfg = Config::load(&path)?;
            let oauth = credential_kind();
            let api = Linear::new(oauth.1, oauth.0)?;
            server::serve(Gateway::new(api)?, cfg).await
        }
        Command::Doctor { json, online } => doctor(&path, json, online).await,
        Command::Config {
            command: ConfigCommand::Check,
        } => config_check(&path),
        Command::SelfInstall {
            bundle,
            home,
            bin_dir,
            adopt_existing,
        } => self_install(&bundle, &home, &bin_dir, adopt_existing),
        Command::Releases {
            command:
                ReleaseCommand::Use {
                    version,
                    home,
                    bin_dir,
                },
        } => match family_delivery::use_version(&home, &bin_dir, &version) {
            Ok(manifest) => {
                print_manifest("activated", &manifest);
                Ok(())
            }
            Err(e) => Err(Fault::new("INSTALL_FAILED", format!("{e}"))),
        },
    }
}
/// Print one install/activation outcome as compact JSON; never a secret.
#[allow(
    clippy::print_stdout,
    reason = "CLI branch; stdout carries the result, never MCP protocol"
)]
fn print_manifest(action: &str, manifest: &family_delivery::Manifest) {
    match serde_json::to_string_pretty(&serde_json::json!({
        "action": action,
        "product": manifest.product,
        "version": manifest.version,
        "target": manifest.target,
        "state_schema": manifest.state_schema,
        "sha256": manifest.sha256,
    })) {
        Ok(text) => println!("{text}"),
        Err(_) => eprintln!("install: cannot serialize result"),
    }
}
/// Install a verified bundle; adoption is explicit, identity-checked and
/// backed up, and nothing outside the product home's standalone tree and the
/// managed launcher is ever written.
#[allow(
    clippy::print_stdout,
    reason = "CLI branch; stdout carries the result, never MCP protocol"
)]
fn self_install(bundle: &Path, home: &Path, bin_dir: &Path, adopt_existing: bool) -> Result<()> {
    let adopted = if adopt_existing {
        match family_delivery::adopt_legacy_launcher(bundle, home, bin_dir) {
            Ok((manifest, adoption)) => {
                println!(
                    "Adopted legacy {} executable; backup: {} (sha256 {})",
                    adoption.version,
                    adoption.backup.display(),
                    adoption.sha256
                );
                Some(manifest)
            }
            Err(e) => return Err(Fault::new("ADOPTION_REFUSED", format!("{e}"))),
        }
    } else {
        None
    };
    let manifest = match adopted {
        Some(manifest) => manifest,
        None => family_delivery::install(bundle, home, bin_dir)
            .map_err(|e| Fault::new("INSTALL_FAILED", format!("{e}")))?,
    };
    print_manifest("installed", &manifest);
    Ok(())
}
/// Read the Linear credential from its environment sources without printing it;
/// returns whether an OAuth token (vs API key) was found and its value.
fn credential_kind() -> (bool, Option<String>) {
    let oauth = std::env::var("LINEAR_OAUTH_TOKEN")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let token = oauth.clone().or_else(|| {
        std::env::var("LINEAR_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
    });
    (oauth.is_some(), token)
}
/// One read-only doctor check result; detail never contains a secret value.
struct Check {
    /// Stable check name.
    name: &'static str,
    /// ok, warning, failed or not_checked.
    status: &'static str,
    /// Safe human explanation.
    detail: String,
}
/// Local read-only checks that require no credentials and no network.
fn local_checks(path: &Path) -> Vec<Check> {
    let mut checks = Vec::new();
    match Catalog::new() {
        Ok(c) => checks.push(Check {
            name: "catalog",
            status: "ok",
            detail: format!("{} embedded tools with compiled validators", c.tools.len()),
        }),
        Err(e) => checks.push(Check {
            name: "catalog",
            status: "failed",
            detail: format!("embedded catalogue invalid: {}", e.message),
        }),
    }
    checks.push(Check {
        name: "presentation",
        status: if render::presentation_ready() {
            "ok"
        } else {
            "failed"
        },
        detail: "strict MiniJinja environment, fuel-bounded, rendered into a bounded buffer"
            .to_owned(),
    });
    match Config::load(path) {
        Ok(_) => checks.push(Check {
            name: "config",
            status: "ok",
            detail: format!(
                "{} valid (legacy protected listen/token format kept as documented compatibility)",
                path.display()
            ),
        }),
        Err(e) if e.code == "CONFIG_MISSING" => checks.push(Check {
            name: "config",
            status: "warning",
            detail: format!(
                "no configuration at {}; run init-config to create one",
                path.display()
            ),
        }),
        Err(e) => checks.push(Check {
            name: "config",
            status: "failed",
            detail: format!("{}: {}", e.code, e.message),
        }),
    }
    let (_, token) = credential_kind();
    checks.push(Check {
        name: "linear_credentials",
        status: if token.is_some() { "ok" } else { "warning" },
        detail: if token.is_some() {
            "LINEAR_OAUTH_TOKEN or LINEAR_API_KEY is present (value not read)".to_owned()
        } else {
            "no Linear credential in the environment; only local checks can run".to_owned()
        },
    });
    match std::process::Command::new("git").arg("--version").output() {
        Ok(output) if output.status.success() => checks.push(Check {
            name: "git",
            status: "ok",
            detail: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        }),
        _ => checks.push(Check {
            name: "git",
            status: "not_checked",
            detail: "git executable unavailable on PATH".to_owned(),
        }),
    }
    checks
}
/// Report local (and optionally online) health. Exit 2 when a mandatory local
/// check fails or an explicitly requested online check fails; exit 3 when the
/// report itself cannot be produced.
#[allow(
    clippy::print_stdout,
    reason = "CLI diagnostics branch; stdout carries the report, never MCP protocol"
)]
async fn doctor(path: &Path, json: bool, online: bool) -> Result<()> {
    let mut checks = local_checks(path);
    if online {
        let (is_oauth, token) = credential_kind();
        let status = match token {
            None => (
                "failed",
                "doctor --online requires LINEAR_OAUTH_TOKEN or LINEAR_API_KEY".to_owned(),
            ),
            Some(token) => match Linear::new(Some(token), is_oauth) {
                Err(e) => ("failed", format!("{}: {}", e.code, e.message)),
                Ok(api) => match api.call("QViewer", json!({})).await {
                    Ok(viewer) => (
                        "ok",
                        format!(
                            "authenticated as {}",
                            viewer["name"].as_str().unwrap_or("the workspace viewer")
                        ),
                    ),
                    Err(e) => ("failed", format!("{}: {}", e.code, e.message)),
                },
            },
        };
        checks.push(Check {
            name: "linear_viewer",
            status: status.0,
            detail: status.1,
        });
    }
    let failed = checks
        .iter()
        .any(|c| c.status == "failed" && c.name != "linear_credentials");
    let report = json!({
        "product": env!("CARGO_PKG_NAME"),
        "version": env!("CARGO_PKG_VERSION"),
        "target": format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
        "online": online,
        "checks": checks.iter().map(|c| json!({
            "name": c.name, "status": c.status, "detail": c.detail,
        })).collect::<Vec<_>>(),
    });
    if json {
        match serde_json::to_string_pretty(&report) {
            Ok(text) => println!("{text}"),
            Err(_) => {
                eprintln!("doctor: cannot serialize report");
                std::process::exit(3);
            }
        }
    } else {
        println!(
            "{} {} ({})",
            report["product"], report["version"], report["target"]
        );
        for check in &checks {
            println!("{}: {} — {}", check.name, check.status, check.detail);
        }
    }
    if failed {
        std::process::exit(2);
    }
    Ok(())
}
/// Validate the protected config without network access or migration.
#[allow(
    clippy::print_stdout,
    reason = "CLI branch; stdout carries the result, never MCP protocol"
)]
fn config_check(path: &Path) -> Result<()> {
    match Config::load(path) {
        Ok(cfg) => {
            println!("config ok: {} (loopback {})", path.display(), cfg.listen);
            Ok(())
        }
        Err(e) => {
            println!("config invalid: {}: {}", e.code, e.message);
            std::process::exit(2);
        }
    }
}
