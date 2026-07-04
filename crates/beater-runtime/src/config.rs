use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

pub const BASE_URL_ENV: &str = "BEATER_BASE_URL";

/// Parsed `beater.toml`.
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub name: String,
    pub port: u16,
    pub host: std::net::IpAddr,
    /// Public URL advertised in crawl and remote-agent metadata.
    pub base_url: Option<String>,
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
    base_url: Option<String>,
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
        let base_url = raw
            .app
            .base_url
            .as_deref()
            .map(normalize_base_url)
            .transpose()
            .with_context(|| format!("invalid [app].base_url in {}", path.display()))?;
        Ok(Self {
            name: raw.app.name,
            port: raw.app.port,
            host: raw.app.host,
            base_url,
            python_venv: raw.python.venv.map(|v| app_dir.join(v)),
            app_dir: app_dir.to_path_buf(),
        })
    }

    pub fn public_base_url(
        &self,
        host: std::net::IpAddr,
        port: u16,
        base_url_override: Option<&str>,
    ) -> Result<String> {
        let env_base_url = std::env::var(BASE_URL_ENV).ok();
        resolve_public_base_url(
            host,
            port,
            base_url_override,
            env_base_url.as_deref(),
            self.base_url.as_deref(),
        )
    }
}

fn resolve_public_base_url(
    host: std::net::IpAddr,
    port: u16,
    base_url_override: Option<&str>,
    env_base_url: Option<&str>,
    config_base_url: Option<&str>,
) -> Result<String> {
    let raw = base_url_override.or(env_base_url).or(config_base_url);
    match raw {
        Some(value) => normalize_base_url(value),
        None => Ok(default_base_url(host, port)),
    }
}

fn normalize_base_url(value: &str) -> Result<String> {
    let value = value.trim().trim_end_matches('/');
    let url = deno_core::url::Url::parse(value).context("base URL must be absolute")?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("base URL scheme must be http or https");
    }
    if url.host_str().is_none() {
        bail!("base URL must include a host");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("base URL must not include credentials");
    }
    if url.query().is_some() {
        bail!("base URL must not include a query string");
    }
    if url.fragment().is_some() {
        bail!("base URL must not include a fragment");
    }
    Ok(value.to_string())
}

fn default_base_url(host: std::net::IpAddr, port: u16) -> String {
    match host {
        std::net::IpAddr::V4(_) => format!("http://{host}:{port}"),
        std::net::IpAddr::V6(_) => format!("http://[{host}]:{port}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{default_base_url, normalize_base_url, resolve_public_base_url};
    use std::net::{IpAddr, Ipv6Addr};

    #[test]
    fn base_url_normalization_trims_trailing_slashes() {
        assert_eq!(
            normalize_base_url("https://example.com/app///").unwrap(),
            "https://example.com/app"
        );
    }

    #[test]
    fn base_url_requires_http_or_https() {
        assert!(normalize_base_url("file:///tmp/app").is_err());
        assert!(normalize_base_url("localhost:3000").is_err());
    }

    #[test]
    fn base_url_rejects_credentials_query_and_fragment() {
        assert!(normalize_base_url("https://user:pass@example.com").is_err());
        assert!(normalize_base_url("https://example.com/app?tenant=1").is_err());
        assert!(normalize_base_url("https://example.com/app#section").is_err());
    }

    #[test]
    fn default_base_url_formats_ipv6_hosts() {
        let host = IpAddr::V6(Ipv6Addr::LOCALHOST);
        assert_eq!(default_base_url(host, 3000), "http://[::1]:3000");
    }

    #[test]
    fn public_base_url_prefers_override_then_env_then_config() {
        let resolved = resolve_public_base_url(
            "127.0.0.1".parse().unwrap(),
            3000,
            Some("https://cli.example"),
            Some("https://env.example"),
            Some("https://config.example"),
        )
        .unwrap();
        assert_eq!(resolved, "https://cli.example");

        let resolved = resolve_public_base_url(
            "127.0.0.1".parse().unwrap(),
            3000,
            None,
            Some("https://env.example"),
            Some("https://config.example"),
        )
        .unwrap();
        assert_eq!(resolved, "https://env.example");

        let resolved = resolve_public_base_url(
            "127.0.0.1".parse().unwrap(),
            3000,
            None,
            None,
            Some("https://config.example"),
        )
        .unwrap();
        assert_eq!(resolved, "https://config.example");
    }
}
