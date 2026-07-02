use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
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
    /// Scaffold a new app from the built-in hello template
    New {
        /// Destination app directory
        app: PathBuf,
    },
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
        /// Allow binding beyond loopback without BEATER_MCP_TOKEN.
        #[arg(long)]
        allow_unauthenticated_remote: bool,
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
        Command::New { app } => scaffold(&app),
        Command::Dev {
            app,
            host,
            port,
            base_url,
            allow_unauthenticated_remote,
        } => beater_runtime::dev(&app, port, host, base_url, allow_unauthenticated_remote),
        Command::Agent { command } => match command {
            AgentCommand::Run { app, name, prompt } => {
                let config = beater_runtime::load_agent_config(&app, &name)?;
                let app_config = beater_runtime::AppConfig::load(&app)?;
                beater_agent::run(
                    &app,
                    &name,
                    config,
                    app_config.python_venv,
                    app_config.beatbox,
                    &prompt,
                )
            }
            AgentCommand::Resume { app, run_id } => {
                let app_config = beater_runtime::AppConfig::load(&app)?;
                beater_agent::resume(
                    &app,
                    &run_id,
                    app_config.python_venv,
                    app_config.beatbox,
                    |agent| beater_runtime::load_agent_config(&app, agent),
                )
            }
            AgentCommand::Runs { app } => beater_agent::list_runs(&app),
        },
        Command::Doctor { app } => doctor(&app),
    }
}

const TEMPLATE_FILES: &[(&str, &str)] = &[
    (
        "beater.toml",
        include_str!("../../../examples/hello/beater.toml"),
    ),
    (
        "app/routes/index.tsx",
        include_str!("../../../examples/hello/app/routes/index.tsx"),
    ),
    (
        "app/routes/index.client.ts",
        include_str!("../../../examples/hello/app/routes/index.client.ts"),
    ),
    (
        "app/routes/api/health.ts",
        include_str!("../../../examples/hello/app/routes/api/health.ts"),
    ),
    (
        "app/routes/api/boom.ts",
        include_str!("../../../examples/hello/app/routes/api/boom.ts"),
    ),
    (
        "agents/support/agent.ts",
        include_str!("../../../examples/hello/agents/support/agent.ts"),
    ),
    (
        "agents/support/tools/summarize_numbers.py",
        include_str!("../../../examples/hello/agents/support/tools/summarize_numbers.py"),
    ),
    (
        "agents/support/tools/slow_summarize.py",
        include_str!("../../../examples/hello/agents/support/tools/slow_summarize.py"),
    ),
    (
        "agents/support/tools/slow_summarize_once.py",
        include_str!("../../../examples/hello/agents/support/tools/slow_summarize_once.py"),
    ),
    (
        "agents/support/tools/fib.wat",
        include_str!("../../../examples/hello/agents/support/tools/fib.wat"),
    ),
];

fn scaffold(app: &Path) -> Result<()> {
    if app.exists() {
        if !app.is_dir() {
            bail!(
                "destination exists and is not a directory: {}",
                app.display()
            );
        }
        if app
            .read_dir()
            .with_context(|| format!("read {}", app.display()))?
            .next()
            .is_some()
        {
            bail!("destination is not empty: {}", app.display());
        }
    }

    let app_name = app
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("beater-app");

    for (relative_path, contents) in TEMPLATE_FILES {
        let path = app.join(relative_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let contents = if *relative_path == "beater.toml" {
            contents.replace(
                "name = \"hello\"",
                &format!("name = \"{}\"", toml_basic_string(app_name)),
            )
        } else {
            contents.to_string()
        };
        std::fs::write(&path, contents).with_context(|| format!("write {}", path.display()))?;
    }

    println!("created {}", app.display());
    println!("next: beater dev {}", app.display());
    Ok(())
}

fn toml_basic_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            ch if ch.is_control() => out.push_str(&format!("\\u{:04X}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out
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
            println!("  beatbox: {}", config.beatbox.url);
            if config.beatbox.api_key.is_some() {
                println!("  beatbox auth: bearer auth enabled");
            } else {
                println!("  beatbox auth: no bearer token configured");
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
