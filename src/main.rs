//! Administrative CLI, authenticated HTTP gateway, and stdio bridge entrypoint.

use agent_tasks_linear::{
    admin,
    config::{self, Config},
    gateway::Gateway,
    linear::Linear,
    model::{Fault, Principal, Result, Role},
    records::{Signer, Store},
    server,
};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Agent-Tasks-Linear command line. Secrets are read from a protected file or environment.
#[derive(Parser)]
#[command(version, about = "Rust workflow MCP over Linear")]
struct Cli {
    /// Private configuration file; defaults to ATL_CONFIG or the user configuration directory.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Administrative or transport operation to perform.
    #[command(subcommand)]
    command: Command,
}
/// Closed CLI surface; every external mutation requires the explicit bootstrap-apply command.
#[derive(Subcommand)]
enum Command {
    /// Create a private owner binding and signing secret without printing credentials.
    InitConfig,
    /// Add a protected worker/root/observer binding, then restart the gateway to activate it.
    AddBinding {
        /// Endpoint suffix and local stdio binding name.
        #[arg(long)]
        name: String,
        /// Logical principal distinct from implementation/reviewer peers.
        #[arg(long)]
        principal: String,
        /// Role name: owner, root, lead, helper, decomposer, reviewer, integrator or observer.
        #[arg(long)]
        role: String,
        /// Allowed product UUID; repeat for multiple products.
        #[arg(long, required = true)]
        product: Vec<String>,
        /// Current assignment UUID returned by assign or review-open.
        #[arg(long)]
        assignment: Option<String>,
        /// Current assignment generation returned by assign or transfer.
        #[arg(long)]
        generation: Option<u64>,
        /// Product policy epoch fixed to this binding.
        #[arg(long, default_value_t = 1)]
        epoch: u64,
    },
    /// Start the one loopback MCP writer; missing Linear token still permits tool discovery.
    Serve,
    /// Connect a standard stdio MCP client to the existing HTTP writer.
    Stdio {
        /// Configured binding name.
        #[arg(long, default_value = "owner")]
        binding: String,
    },
    /// Check local configuration and, when token is present, read-only Linear access.
    Doctor,
    /// Write a non-secret reviewed provisioning plan; makes no API calls.
    BootstrapPlan {
        /// UUID of the dedicated Linear team.
        #[arg(long)]
        team_id: String,
        /// Human product name.
        #[arg(long)]
        name: String,
        /// New JSON plan path; existing files are refused.
        #[arg(long)]
        out: PathBuf,
    },
    /// Apply an already saved plan with reserved IDs to the selected Linear team.
    BootstrapApply {
        /// Previously inspected JSON plan.
        #[arg(long)]
        plan: PathBuf,
    },
}
/// Run the CLI and print safe errors to stderr, keeping MCP stdout exclusively protocol data.
#[tokio::main]
async fn main() {
    if let Err(error) = run(Cli::parse()).await {
        eprintln!("{error}");
        std::process::exit(1)
    }
}
/// Dispatch one CLI operation with secrets loaded only when needed.
async fn run(cli: Cli) -> Result<()> {
    let path = cli.config.unwrap_or_else(config::default_path);
    if matches!(cli.command, Command::InitConfig) {
        Config::initialize(&path)?;
        println!("Created private configuration: {}", path.display());
        return Ok(());
    }
    if let Command::AddBinding {
        name,
        principal,
        role,
        product,
        assignment,
        generation,
        epoch,
    } = cli.command
    {
        let role: Role = serde_json::from_value(serde_json::Value::String(role))
            .map_err(|_| Fault::new("CONFIG_INVALID", "Unknown binding role"))?;
        Config::add_binding(
            &path,
            name,
            Principal {
                id: principal,
                role,
                products: product,
                assignment_id: assignment,
                generation,
                epoch,
            },
        )?;
        println!(
            "Added private binding. Restart the gateway to activate the updated configuration."
        );
        return Ok(());
    }
    if let Command::BootstrapPlan { team_id, name, out } = cli.command {
        use std::io::Write;
        let plan = admin::bootstrap_plan(&team_id, &name)?;
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&out)
            .map_err(|_| Fault::new("PLAN_EXISTS", "Plan path exists or cannot be created"))?;
        file.write_all(serde_json::to_string_pretty(&plan).unwrap().as_bytes())
            .map_err(|_| Fault::new("PLAN_INVALID", "Cannot write bootstrap plan"))?;
        println!("Prepared plan: {} (no Linear writes)", out.display());
        return Ok(());
    }
    let config = Config::load(&path)?;
    if let Command::Stdio { binding } = cli.command {
        return server::stdio(&config, config.binding(&binding)?).await;
    }
    let oauth = std::env::var("LINEAR_OAUTH_TOKEN")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let token = oauth.clone().or_else(|| {
        std::env::var("LINEAR_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
    });
    let linear = Linear::new(token, oauth.is_some())?;
    if matches!(cli.command, Command::Doctor) {
        println!(
            "{}",
            serde_json::to_string_pretty(&admin::doctor(&linear, &config).await?).unwrap()
        );
        return Ok(());
    }
    let store = Store {
        linear,
        signer: Signer::new(&config.signing_key)?,
    };
    match cli.command {
        Command::Serve => server::serve(Gateway::new(store)?, config).await,
        Command::BootstrapApply { plan } => {
            let data = std::fs::read(plan)
                .map_err(|_| Fault::new("PLAN_INVALID", "Cannot read bootstrap plan"))?;
            let plan = serde_json::from_slice(&data)
                .map_err(|_| Fault::new("PLAN_INVALID", "Invalid bootstrap plan JSON"))?;
            let owner = &config
                .bindings
                .iter()
                .find(|b| b.principal.role == agent_tasks_linear::model::Role::Owner)
                .ok_or_else(|| Fault::new("UNAUTHORIZED", "No owner binding"))?
                .principal;
            println!(
                "{}",
                serde_json::to_string_pretty(&admin::bootstrap(&store, owner, &plan).await?)
                    .unwrap()
            );
            Ok(())
        }
        _ => unreachable!(),
    }
}
