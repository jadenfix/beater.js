use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

/// Parsed `beater.toml`.
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub name: String,
    pub port: u16,
    pub host: std::net::IpAddr,
    /// Path to a Python venv whose site-packages are attached at runtime.
    pub python_venv: Option<PathBuf>,
    pub app_dir: PathBuf,
}

#[derive(Deserialize)]
struct RawConfig {
    app: RawApp,
    #[serde(default)]
    python: RawPython,
}

#[derive(Deserialize)]
struct RawApp {
    name: String,
    #[serde(default = "default_port")]
    port: u16,
    #[serde(default = "default_host")]
    host: std::net::IpAddr,
}

#[derive(Deserialize, Default)]
struct RawPython {
    venv: Option<PathBuf>,
}

fn default_port() -> u16 {
    3000
}

fn default_host() -> std::net::IpAddr {
    std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
}

impl AppConfig {
    pub fn load(app_dir: &Path) -> Result<Self> {
        // file:// module specifiers require absolute paths
        let app_dir = &app_dir
            .canonicalize()
            .with_context(|| format!("app dir not found: {}", app_dir.display()))?;
        let path = app_dir.join("beater.toml");
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("no beater.toml at {}", path.display()))?;
        let raw: RawConfig = toml::from_str(&text)
            .with_context(|| format!("invalid beater.toml at {}", path.display()))?;
        Ok(Self {
            name: raw.app.name,
            port: raw.app.port,
            host: raw.app.host,
            python_venv: raw.python.venv.map(|v| app_dir.join(v)),
            app_dir: app_dir.to_path_buf(),
        })
    }
}
