use anyhow::Result;
use clap::{Parser, Subcommand};


mod commands;

#[derive(Parser)]
#[command(name = "kumo")]
#[command(version)]
#[command(about = "A security-first, space-efficient package manager", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    #[command(alias = "i")]
    Install {
        #[arg(long)]
        log: bool,
    },
    #[command(alias = "a")]
    Add {
        name: String,
        #[arg(short, long)]
        dev: bool,
        #[arg(short, long)]
        global: bool,
        #[arg(long)]
        log: bool,
    },
    #[command(alias = "rm")]
    #[command(alias = "un")]
    #[command(alias = "uninstall")]
    Remove {
        name: String,
    },
    Scan,
    #[command(alias = "st")]
    Stats,
    Prune {
        #[command(subcommand)]
        subcommand: commands::prune::PruneSubcommand,
    },
    #[command(alias = "dr")]
    Doctor,
    #[command(alias = "ex")]
    Explain {
        name: String,
    },
    Config {
        #[command(subcommand)]
        subcommand: ConfigSubcommand,
    },
    Workspaces,
    Patch {
        name: String,
    },
    Timeline,
    Graph,
    Sandbox {
        script: String,
    },
    Update {
        #[arg(long)]
        pre: bool,
        version: Option<String>,
    },
    #[command(alias = "up")]
    Upgrade {
        /// Specific packages to upgrade (empty = all) or upgrade type (major, minor, patch/parch)
        packages: Vec<String>,
        /// Upgrade to the absolute latest version, ignoring semver ranges (now default)
        #[arg(short = 'L', long)]
        latest: bool,
        /// Only upgrade dependencies (skip devDependencies)
        #[arg(long)]
        prod: bool,
        /// Only upgrade devDependencies (skip dependencies)
        #[arg(long)]
        dev: bool,
        /// Save upgraded versions as fixed (without ^ prefix)
        #[arg(short = 'F', long)]
        fixed: bool,
        /// Show available updates without applying them
        #[arg(short = 'n', long)]
        dry_run: bool,
        /// Show detailed installation progress
        #[arg(long)]
        log: bool,
    },
    #[command(external_subcommand)]
    External(Vec<String>),
}



#[derive(Subcommand)]
pub enum ConfigSubcommand {
    Init,
    Default {
        setting: String,
        value: String,
    },
}

mod common;

#[tokio::main]
async fn main() -> Result<()> {


    tokio::spawn(async {
        if tokio::signal::ctrl_c().await.is_ok() {
            if tokio::signal::ctrl_c().await.is_ok() {
                std::process::exit(1);
            }
        }
    });

    let cli = Cli::parse();

    let update_check_handle = if !matches!(cli.command, Commands::Update { .. }) {
        Some(tokio::spawn(common::check_for_new_version()))
    } else {
        None
    };

    let (store, security, resolver) = common::init_components().await?;

    let kumo_json_path = std::env::current_dir()?.join("kumo.json");
    let pkg_json_path = std::env::current_dir()?.join("package.json");
    let config_path = if kumo_json_path.exists() {
        Some(kumo_json_path)
    } else if pkg_json_path.exists() {
        Some(pkg_json_path)
    } else {
        None
    };

    match cli.command {
        Commands::Install { log } => {
            let config_path = config_path.ok_or_else(|| anyhow::anyhow!("Neither kumo.json nor package.json found in current directory"))?;
            commands::install::execute(&store, &resolver, &security, log, config_path).await?;
        }
        Commands::Add {
            name,
            dev,
            global,
            log,
        } => {
            commands::add::execute(&store, &resolver, &security, name, dev, global, log, config_path).await?;
        }
        Commands::Remove { name } => {
            commands::remove::execute(&store, &resolver, &security, name, config_path).await?;
        }
        Commands::Scan => {
            commands::scan::execute(&security).await?;
        }
        Commands::Stats => {
            commands::stats::execute(&store).await?;
        }
        Commands::Prune { subcommand } => {
            commands::prune::execute(&store, subcommand).await?;
        }
        Commands::Doctor => {
            commands::doctor::execute(&store).await?;
        }
        Commands::Explain { name } => {
            commands::explain::execute(&name).await?;
        }
        Commands::Workspaces => {
            commands::workspaces::execute().await?;
        }
        Commands::Patch { name } => {
            commands::patch::execute(name).await?;
        }
        Commands::Timeline => {
            commands::timeline::execute().await?;
        }
        Commands::Graph => {
            commands::graph::execute().await?;
        }
        Commands::Sandbox { script } => {
            commands::sandbox::execute(script).await?;
        }
        Commands::Update { pre, version } => commands::update::execute(pre, version).await?,
        Commands::Upgrade { packages, latest, prod, dev, fixed, dry_run, log } => {
            let config_path = config_path.ok_or_else(|| anyhow::anyhow!("Neither kumo.json nor package.json found in current directory"))?;
            commands::upgrade::execute(&store, &resolver, &security, packages, latest, prod, dev, fixed, dry_run, log, config_path).await?;
        }
        Commands::Config { subcommand } => {
            commands::config::execute(subcommand).await?;
        }
        Commands::External(args) => {
            if args.is_empty() {
                anyhow::bail!("No script specified");
            }
            let script_name = &args[0];
            let script_args = &args[1..];
            commands::run::execute(script_name, script_args.to_vec()).await?;
        }
    }

    if let Some(handle) = update_check_handle {
        if let Ok(Some(new_version)) = handle.await {
            common::print_update_banner(&new_version);
        }
    }

    Ok(())
}
