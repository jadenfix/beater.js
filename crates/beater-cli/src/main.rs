use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "beater",
    version,
    about = "beater.js — one runtime for the agent-first web"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve an app with file-based routes and hot reload
    Dev {
        /// App directory (contains beater.toml)
        #[arg(default_value = ".")]
        app: PathBuf,
        /// Override the bind host from beater.toml
        #[arg(long)]
        host: Option<std::net::IpAddr>,
        /// Override the port from beater.toml
        #[arg(long)]
        port: Option<u16>,
        /// Public base URL advertised in agent/crawl metadata
        #[arg(long)]
        base_url: Option<String>,
    },
    /// Run, resume, and inspect durable agent runs
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    /// Check the environment: embedded Python, venv wiring, V8
    Doctor {
        /// App directory (contains beater.toml)
        #[arg(default_value = ".")]
        app: PathBuf,
    },
}

#[derive(Subcommand)]
enum AgentCommand {
    /// Start a new run of an agent
    Run {
        /// App directory (contains beater.toml)
        #[arg(long, default_value = ".")]
        app: PathBuf,
        /// Agent name (directory under agents/)
        name: String,
        /// The user prompt
        prompt: String,
    },
    /// Resume a crashed or interrupted run from its journal
    Resume {
        #[arg(long, default_value = ".")]
        app: PathBuf,
        run_id: String,
    },
    /// List runs recorded in the journal
    Runs {
        #[arg(long, default_value = ".")]
        app: PathBuf,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Dev {
            app,
            host,
            port,
            base_url,
        } => beater_runtime::dev(&app, port, host, base_url),
        Command::Agent { command } => match command {
            AgentCommand::Run { app, name, prompt } => {
                let config = beater_runtime::load_agent_config(&app, &name)?;
                let venv = beater_runtime::AppConfig::load(&app)?.python_venv;
                beater_agent::run(&app, &name, config, venv, &prompt)
            }
            AgentCommand::Resume { app, run_id } => {
                let venv = beater_runtime::AppConfig::load(&app)?.python_venv;
                beater_agent::resume(&app, &run_id, venv, |agent| {
                    beater_runtime::load_agent_config(&app, agent)
                })
            }
            AgentCommand::Runs { app } => beater_agent::list_runs(&app),
        },
        Command::Doctor { app } => doctor(&app),
    }
}

fn doctor(app: &std::path::Path) -> Result<()> {
    println!("beater doctor");
    println!("  app dir: {}", app.display());
    match beater_runtime::AppConfig::load(app) {
        Ok(config) => {
            println!("  app:     {}", config.name);
            println!("  bind:    {}:{}", config.host, config.port);
            match config.public_base_url(config.host, config.port, None) {
                Ok(base_url) => println!("  public:  {base_url}"),
                Err(e) => println!("  public:  INVALID — {e}"),
            }
            match &config.python_venv {
                Some(venv) => {
                    println!("  venv:    {}", venv.display());
                    match beater_py::check_venv(venv) {
                        Ok(site_packages) => {
                            println!("  venv ok: {}", site_packages.display());
                        }
                        Err(e) => {
                            println!("  venv:    MISMATCH — {e}");
                        }
                    }
                }
                None => println!("  venv:    none configured (stdlib-only Python tools)"),
            }
        }
        Err(e) => println!("  app:     UNAVAILABLE — {e:#}"),
    }
    match beater_py::python_info() {
        Ok(info) => println!("  python:  {info}"),
        Err(e) => println!("  python:  UNAVAILABLE — {e}"),
    }
    match std::env::var("PYO3_PYTHON") {
        Ok(path) => println!("  shell:   PYO3_PYTHON={path}"),
        Err(_) => println!("  shell:   PYO3_PYTHON not set"),
    }
    let mcp_access = beater_runtime::McpAccessConfig::from_env();
    if mcp_access.auth_required() {
        println!("  mcp:     bearer auth enabled");
    } else {
        println!("  mcp:     no bearer token configured");
    }
    if !mcp_access.trusted_origins().is_empty() {
        println!("  origins: {}", mcp_access.trusted_origins().join(", "));
    }
    println!("  v8:      {}", beater_runtime::v8_version());
    Ok(())
}
