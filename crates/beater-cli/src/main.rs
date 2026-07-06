use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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
    /// Build a runnable app bundle with the beater binary, assets, and Docker context
    Build {
        /// App directory (contains beater.toml)
        #[arg(default_value = ".")]
        app: PathBuf,
        /// Output directory for the bundle
        #[arg(long)]
        out: Option<PathBuf>,
        /// Remove an existing output directory before writing the bundle
        #[arg(long)]
        force: bool,
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
        Command::Build { app, out, force } => build_bundle(&app, out.as_deref(), force),
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
        "app/client/counter.ts",
        include_str!("../../../examples/hello/app/client/counter.ts"),
    ),
    (
        "app/routes/index.server.tsx",
        include_str!("../../../examples/hello/app/routes/index.server.tsx"),
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
        "app/routes/api/actions/contact.ts",
        include_str!("../../../examples/hello/app/routes/api/actions/contact.ts"),
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

fn build_bundle(app: &Path, out: Option<&Path>, force: bool) -> Result<()> {
    let config = beater_runtime::AppConfig::load(app)?;
    let out_dir = resolve_build_output(&config.app_dir, &config.name, out)?;
    let output_plan = validate_build_output(&out_dir, &config.app_dir, force)?;
    let staging = StagingDir::new(&out_dir)?;

    let app_out = staging.path.join("app");
    let bin_out = staging.path.join("bin");
    std::fs::create_dir_all(&app_out).with_context(|| format!("create {}", app_out.display()))?;
    std::fs::create_dir_all(&bin_out).with_context(|| format!("create {}", bin_out.display()))?;

    copy_app_tree(&config.app_dir, &app_out)?;

    let beater_out = bin_out.join("beater");
    copy_current_exe(&beater_out)?;
    write_executable(&staging.path.join("run.sh"), launcher_script())?;
    std::fs::write(staging.path.join("Dockerfile"), dockerfile())
        .with_context(|| format!("write {}", staging.path.join("Dockerfile").display()))?;
    std::fs::write(staging.path.join(".dockerignore"), dockerignore())
        .with_context(|| format!("write {}", staging.path.join(".dockerignore").display()))?;
    std::fs::write(
        staging.path.join("beater-build.json"),
        build_manifest(&config.name),
    )
    .with_context(|| format!("write {}", staging.path.join("beater-build.json").display()))?;
    std::fs::write(staging.path.join("README.md"), build_readme(&config.name))
        .with_context(|| format!("write {}", staging.path.join("README.md").display()))?;

    staging.install(&out_dir, &config.app_dir, output_plan)?;

    println!("built {}", out_dir.display());
    println!(
        "run: BEATER_HOST=127.0.0.1 BEATER_PORT={} {}/run.sh",
        config.port,
        out_dir.display()
    );
    println!(
        "container: docker build -t {} {} && docker run --rm -e BEATER_MCP_TOKEN=... -p {}:{} {}",
        shell_token(&config.name),
        out_dir.display(),
        config.port,
        config.port,
        shell_token(&config.name)
    );
    Ok(())
}

fn resolve_build_output(app_dir: &Path, app_name: &str, out: Option<&Path>) -> Result<PathBuf> {
    let raw = out
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_build_output(app_dir, app_name));
    let absolute = absolute_lexical_path(&raw)?;
    if absolute.file_name().is_none() {
        bail!("build output must name a directory: {}", raw.display());
    }
    canonical_output_path(&absolute)
}

fn default_build_output(app_dir: &Path, app_name: &str) -> PathBuf {
    let parent = app_dir.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!("{}-beater-bundle", safe_path_segment(app_name)))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BuildOutputPlan {
    Absent,
    EmptyDir,
    ReplaceBundle,
}

fn validate_build_output(out_dir: &Path, app_dir: &Path, force: bool) -> Result<BuildOutputPlan> {
    if out_dir == app_dir || out_dir.starts_with(app_dir) {
        bail!(
            "build output must be outside the app directory to avoid copying or deleting source files: {}",
            out_dir.display()
        );
    }

    if !out_dir.exists() {
        return Ok(BuildOutputPlan::Absent);
    }

    inspect_existing_output(out_dir, app_dir)?;
    if out_dir
        .read_dir()
        .with_context(|| format!("read {}", out_dir.display()))?
        .next()
        .is_none()
    {
        return Ok(BuildOutputPlan::EmptyDir);
    }

    if force {
        ensure_replaceable_build_output(out_dir)?;
        Ok(BuildOutputPlan::ReplaceBundle)
    } else {
        bail!(
            "build output is not empty: {} (pass --force to replace it)",
            out_dir.display()
        );
    }
}

fn inspect_existing_output(out_dir: &Path, app_dir: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(out_dir)
        .with_context(|| format!("inspect {}", out_dir.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("build output must not be a symlink: {}", out_dir.display());
    }
    if !metadata.is_dir() {
        bail!(
            "build output exists and is not a directory: {}",
            out_dir.display()
        );
    }
    let canonical = out_dir
        .canonicalize()
        .with_context(|| format!("inspect {}", out_dir.display()))?;
    if canonical == app_dir || canonical.starts_with(app_dir) {
        bail!(
            "build output resolves inside the app directory: {}",
            out_dir.display()
        );
    }
    Ok(())
}

struct StagingDir {
    path: PathBuf,
    installed: std::cell::Cell<bool>,
}

impl StagingDir {
    fn new(out_dir: &Path) -> Result<Self> {
        let parent = out_dir.parent().with_context(|| {
            format!("build output parent does not exist: {}", out_dir.display())
        })?;
        let name = out_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("bundle");
        let mut path = parent.join(format!(
            ".{name}.tmp-{}-{}",
            std::process::id(),
            current_nanos()?
        ));
        for attempt in 0..100 {
            if attempt > 0 {
                path = parent.join(format!(
                    ".{name}.tmp-{}-{}-{attempt}",
                    std::process::id(),
                    current_nanos()?
                ));
            }
            match std::fs::create_dir(&path) {
                Ok(()) => {
                    return Ok(Self {
                        path,
                        installed: std::cell::Cell::new(false),
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e).with_context(|| format!("create {}", path.display())),
            }
        }
        bail!(
            "could not create a unique build staging directory beside {}",
            out_dir.display()
        );
    }

    fn install(&self, out_dir: &Path, app_dir: &Path, plan: BuildOutputPlan) -> Result<()> {
        let backup = match plan {
            BuildOutputPlan::Absent => {
                if out_dir.exists() {
                    bail!(
                        "build output appeared while building; refusing to replace it without rerunning: {}",
                        out_dir.display()
                    );
                }
                None
            }
            BuildOutputPlan::EmptyDir => {
                inspect_existing_output(out_dir, app_dir)?;
                if out_dir
                    .read_dir()
                    .with_context(|| format!("read {}", out_dir.display()))?
                    .next()
                    .is_some()
                {
                    bail!(
                        "build output changed while building; refusing to replace it without rerunning: {}",
                        out_dir.display()
                    );
                }
                Some(rename_existing_output_to_backup(out_dir)?)
            }
            BuildOutputPlan::ReplaceBundle => {
                inspect_existing_output(out_dir, app_dir)?;
                ensure_replaceable_build_output(out_dir)?;
                Some(rename_existing_output_to_backup(out_dir)?)
            }
        };

        match std::fs::rename(&self.path, out_dir) {
            Ok(()) => {
                self.installed.set(true);
                if let Some(backup) = backup {
                    let _ = std::fs::remove_dir_all(backup);
                }
            }
            Err(e) => {
                if let Some(backup) = backup {
                    let _ = std::fs::rename(&backup, out_dir);
                }
                return Err(e).with_context(|| {
                    format!("move {} to {}", self.path.display(), out_dir.display())
                });
            }
        }
        self.installed.set(true);
        Ok(())
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        if !self.installed.get() {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

fn rename_existing_output_to_backup(out_dir: &Path) -> Result<PathBuf> {
    let backup = unique_sibling_path(out_dir, "old")?;
    std::fs::rename(out_dir, &backup)
        .with_context(|| format!("move {} to {}", out_dir.display(), backup.display()))?;
    Ok(backup)
}

fn unique_sibling_path(out_dir: &Path, label: &str) -> Result<PathBuf> {
    let parent = out_dir
        .parent()
        .with_context(|| format!("build output parent does not exist: {}", out_dir.display()))?;
    let name = out_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("bundle");
    for attempt in 0..100 {
        let suffix = if attempt == 0 {
            String::new()
        } else {
            format!("-{attempt}")
        };
        let path = parent.join(format!(
            ".{name}.{label}-{}-{}{}",
            std::process::id(),
            current_nanos()?,
            suffix
        ));
        if !path.exists() {
            return Ok(path);
        }
    }
    bail!(
        "could not create a unique build {label} path beside {}",
        out_dir.display()
    );
}

fn ensure_replaceable_build_output(out_dir: &Path) -> Result<()> {
    if out_dir
        .read_dir()
        .with_context(|| format!("read {}", out_dir.display()))?
        .next()
        .is_none()
    {
        return Ok(());
    }

    let manifest = out_dir.join("beater-build.json");
    let text = std::fs::read_to_string(&manifest).with_context(|| {
        format!(
            "refusing to replace {} without a beater-build.json marker",
            out_dir.display()
        )
    })?;
    if !text.contains("\"schema\": \"https://beater.js/build-bundle/v1\"") {
        bail!(
            "refusing to replace {} because beater-build.json is not a beater bundle marker",
            out_dir.display()
        );
    }
    Ok(())
}

fn current_nanos() -> Result<u128> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system time before unix epoch")?
        .as_nanos())
}

fn copy_app_tree(src: &Path, dst: &Path) -> Result<()> {
    for entry in std::fs::read_dir(src).with_context(|| format!("read {}", src.display()))? {
        let entry = entry.with_context(|| format!("read entry in {}", src.display()))?;
        let name = entry.file_name();
        if skipped_bundle_entry(&name) {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        let file_type = entry
            .file_type()
            .with_context(|| format!("inspect {}", src_path.display()))?;
        if file_type.is_dir() {
            std::fs::create_dir_all(&dst_path)
                .with_context(|| format!("create {}", dst_path.display()))?;
            copy_app_tree(&src_path, &dst_path)?;
        } else if file_type.is_file() {
            std::fs::copy(&src_path, &dst_path).with_context(|| {
                format!("copy {} to {}", src_path.display(), dst_path.display())
            })?;
        } else if file_type.is_symlink() {
            bail!(
                "cannot bundle symlink {}; replace it with a real file or directory first",
                src_path.display()
            );
        }
    }
    Ok(())
}

fn skipped_bundle_entry(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    matches!(
        name,
        ".beater"
            | ".git"
            | "target"
            | ".DS_Store"
            | ".env"
            | ".envrc"
            | ".npmrc"
            | ".pypirc"
            | ".netrc"
            | ".aws"
            | ".gcloud"
            | ".ssh"
            | "id_rsa"
            | "id_ed25519"
    ) || name.starts_with(".env.")
}

fn copy_current_exe(dst: &Path) -> Result<()> {
    let exe = std::env::current_exe().context("find current beater executable")?;
    std::fs::copy(&exe, dst)
        .with_context(|| format!("copy {} to {}", exe.display(), dst.display()))?;
    make_executable(dst)?;
    Ok(())
}

fn write_executable(path: &Path, contents: &str) -> Result<()> {
    std::fs::write(path, contents).with_context(|| format!("write {}", path.display()))?;
    make_executable(path)?;
    Ok(())
}

fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path)
            .with_context(|| format!("stat {}", path.display()))?
            .permissions();
        permissions.set_mode(permissions.mode() | 0o755);
        std::fs::set_permissions(path, permissions)
            .with_context(|| format!("chmod +x {}", path.display()))?;
    }
    Ok(())
}

fn launcher_script() -> &'static str {
    r#"#!/usr/bin/env sh
set -eu

DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
HOST=${BEATER_HOST:-127.0.0.1}
PORT_VALUE=${BEATER_PORT:-${PORT:-3000}}

if [ -d /Library/Developer/CommandLineTools/Library/Frameworks ]; then
  export DYLD_FRAMEWORK_PATH="${DYLD_FRAMEWORK_PATH:+$DYLD_FRAMEWORK_PATH:}/Library/Developer/CommandLineTools/Library/Frameworks"
fi

if [ "${BEATER_ALLOW_UNAUTHENTICATED_REMOTE:-}" = "1" ]; then
  exec "$DIR/bin/beater" dev "$DIR/app" --host "$HOST" --port "$PORT_VALUE" --allow-unauthenticated-remote "$@"
fi

exec "$DIR/bin/beater" dev "$DIR/app" --host "$HOST" --port "$PORT_VALUE" "$@"
"#
}

fn dockerfile() -> &'static str {
    r#"# bin/beater must match the target image OS and architecture.
FROM python:3.11-slim

WORKDIR /srv/beater
COPY . .
RUN chmod +x ./run.sh ./bin/beater \
    && mkdir -p ./app/.beater \
    && useradd --system --no-create-home --home-dir /srv/beater --shell /usr/sbin/nologin beater \
    && chown -R beater:beater /srv/beater

ENV BEATER_HOST=0.0.0.0
ENV BEATER_PORT=3000
EXPOSE 3000

USER beater
CMD ["./run.sh"]
"#
}

fn dockerignore() -> &'static str {
    r#".git
.beater
.DS_Store
.env
.env.*
.npmrc
.pypirc
.netrc
.aws
.gcloud
.ssh
id_rsa
id_ed25519
"#
}

fn build_manifest(app_name: &str) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"schema\": \"https://beater.js/build-bundle/v1\",\n",
            "  \"app\": {},\n",
            "  \"binary\": \"bin/beater\",\n",
            "  \"appDir\": \"app\",\n",
            "  \"launcher\": \"run.sh\",\n",
            "  \"dockerfile\": \"Dockerfile\"\n",
            "}}\n"
        ),
        json_string(app_name)
    )
}

fn build_readme(app_name: &str) -> String {
    format!(
        r#"# {app_name} beater bundle

Run locally:

```sh
BEATER_HOST=127.0.0.1 BEATER_PORT=3000 ./run.sh
```

Build a container image from a Linux bundle:

```sh
docker build -t {image} .
docker run --rm -e BEATER_MCP_TOKEN=replace-me -p 3000:3000 {image}
```

`BEATER_MCP_TOKEN` is required when the bundle binds to `0.0.0.0`; this keeps the `/mcp` endpoint closed by default on remote surfaces.
"#,
        app_name = app_name,
        image = shell_token(app_name)
    )
}

fn absolute_lexical_path(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("read current directory")?
            .join(path)
    };
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                out.push(component.as_os_str());
            }
        }
    }
    Ok(out)
}

fn canonical_output_path(path: &Path) -> Result<PathBuf> {
    let name = path
        .file_name()
        .with_context(|| format!("build output must name a directory: {}", path.display()))?;
    let parent = path
        .parent()
        .with_context(|| format!("build output parent does not exist: {}", path.display()))?;

    let mut suffix = PathBuf::new();
    let mut cursor = parent;
    while !cursor.exists() {
        let name = cursor
            .file_name()
            .with_context(|| format!("build output parent does not exist: {}", path.display()))?;
        suffix = Path::new(name).join(suffix);
        cursor = cursor
            .parent()
            .with_context(|| format!("build output parent does not exist: {}", path.display()))?;
    }

    let mut output = cursor
        .canonicalize()
        .with_context(|| format!("inspect {}", cursor.display()))?;
    if !suffix.as_os_str().is_empty() {
        output.push(suffix);
    }
    output.push(name);
    Ok(output)
}

fn safe_path_segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    if out.trim_matches(['-', '.', '_']).is_empty() {
        "app".to_string()
    } else {
        out
    }
}

fn shell_token(value: &str) -> String {
    let segment = safe_path_segment(value).to_ascii_lowercase();
    if segment == "app" {
        "beater-app".to_string()
    } else {
        segment
    }
}

fn json_string(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            ch if ch.is_control() => out.push_str(&format!("\\u{:04X}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn doctor(app: &std::path::Path) -> Result<()> {
    let mut failed = false;
    println!("beater doctor");
    println!("  app dir: {}", app.display());
    match beater_runtime::AppConfig::load(app) {
        Ok(config) => {
            println!("  app:     {}", config.name);
            println!("  bind:    {}:{}", config.host, config.port);
            match config.public_base_url(config.host, config.port, None) {
                Ok(base_url) => println!("  public:  {base_url}"),
                Err(e) => {
                    failed = true;
                    println!("  public:  INVALID — {e}");
                }
            }
            match &config.python_venv {
                Some(venv) => {
                    println!("  venv:    {}", venv.display());
                    match beater_py::check_venv(venv) {
                        Ok(site_packages) => {
                            println!("  venv ok: {}", site_packages.display());
                        }
                        Err(e) => {
                            failed = true;
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
        Err(e) => {
            failed = true;
            println!("  app:     UNAVAILABLE — {e:#}");
        }
    }
    match beater_py::python_info() {
        Ok(info) => println!("  python:  {info}"),
        Err(e) => {
            failed = true;
            println!("  python:  UNAVAILABLE — {e}");
        }
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
    if failed {
        bail!("doctor found problems");
    }
    Ok(())
}
