//! Minimal CLI for the authenticated loopback writer and standard stdio bridge.
use agent_tasks_linear::{
    config::{self, Config},
    gateway::Gateway,
    linear::Linear,
    model::{Fault, Result},
    server,
};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
/// Select a protected local configuration and one explicit service operation.
#[derive(Parser)]
#[command(version, about = "Basic native Linear workflow MCP")]
struct Cli {
    /// Private deployment configuration, defaulting to ATL_CONFIG.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Operation; bootstrap/provisioning commands from v1 were removed.
    #[command(subcommand)]
    command: Command,
}
/// Supported lifecycle operations; no workflow actions are hidden in the CLI.
#[derive(Subcommand)]
enum Command {
    /// Generate a new private bearer config; never overwrites existing credentials.
    InitConfig,
    /// Run the single loopback MCP writer.
    Serve,
    /// Bridge stdio to the existing writer at /mcp.
    Stdio,
    /// Read-only credential check returning the authenticated Linear viewer.
    Doctor,
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
async fn run(cli: Cli) -> Result<()> {
    let path = cli.config.unwrap_or_else(config::default_path);
    if matches!(cli.command, Command::InitConfig) {
        Config::initialize(&path)?;
        println!("Created private configuration: {}", path.display());
        return Ok(());
    }
    let cfg = Config::load(&path)?;
    if matches!(cli.command, Command::Stdio) {
        return server::stdio(&cfg).await;
    }
    let oauth = std::env::var("LINEAR_OAUTH_TOKEN")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let token = oauth.clone().or_else(|| {
        std::env::var("LINEAR_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
    });
    let api = Linear::new(token, oauth.is_some())?;
    match cli.command {
        Command::Serve => server::serve(Gateway::new(api)?, cfg).await,
        Command::Doctor => {
            let viewer = api.call("QViewer", serde_json::json!({})).await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&viewer)
                    .map_err(|_| Fault::new("SERIALIZATION", "Cannot format doctor response"))?
            );
            Ok(())
        }
        _ => unreachable!(),
    }
}
