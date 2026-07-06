use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use beater_agent::{BeatboxConfig, DEFAULT_BEATBOX_URL};
use serde::Deserialize;

pub const BASE_URL_ENV: &str = "BEATER_BASE_URL";
pub const BEATBOX_URL_ENV: &str = "BEATBOX_URL";
pub const BEATBOX_API_KEY_ENV: &str = "BEATBOX_API_KEY";

/// Parsed `beater.toml`.
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub name: String,
    pub port: u16,
    pub host: std::net::IpAddr,
    /// Number of JS route isolates to keep hot in `beater dev`.
    pub workers: usize,
    /// Public URL advertised in crawl and remote-agent metadata.
    pub base_url: Option<String>,
    /// Path to a Python venv whose site-packages are attached at runtime.
    pub python_venv: Option<PathBuf>,
    /// Local beatbox daemon used for Tier-4 sandbox tools.
    pub beatbox: BeatboxConfig,
    pub app_dir: PathBuf,
}

#[derive(Deserialize)]
struct RawConfig {
    app: RawApp,
    #[serde(default)]
    python: RawPython,
    #[serde(default)]
    beatbox: RawBeatbox,
}

#[derive(Deserialize)]
struct RawApp {
    name: String,
    #[serde(default = "default_port")]
    port: u16,
    #[serde(default = "default_host")]
    host: std::net::IpAddr,
    #[serde(default = "default_workers")]
    workers: usize,
    base_url: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawPython {
    venv: Option<PathBuf>,
}

#[derive(Deserialize, Default)]
struct RawBeatbox {
    url: Option<String>,
    api_key: Option<String>,
}

fn default_port() -> u16 {
    3000
}

fn default_host() -> std::net::IpAddr {
    std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
}

fn default_workers() -> usize {
    1
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
            workers: raw.app.workers.max(1),
            base_url,
            python_venv: raw.python.venv.map(|v| app_dir.join(v)),
            beatbox: resolve_beatbox_config(
                raw.beatbox.url.as_deref(),
                raw.beatbox.api_key.as_deref(),
            )
            .with_context(|| format!("invalid [beatbox] config in {}", path.display()))?,
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

fn resolve_beatbox_config(
    config_url: Option<&str>,
    config_api_key: Option<&str>,
) -> Result<BeatboxConfig> {
    let env_url = std::env::var(BEATBOX_URL_ENV).ok();
    let env_api_key = std::env::var(BEATBOX_API_KEY_ENV).ok();
    resolve_beatbox_config_with_env(
        config_url,
        config_api_key,
        env_url.as_deref(),
        env_api_key.as_deref(),
    )
}

fn resolve_beatbox_config_with_env(
    config_url: Option<&str>,
    config_api_key: Option<&str>,
    env_url: Option<&str>,
    env_api_key: Option<&str>,
) -> Result<BeatboxConfig> {
    let url = env_url
        .and_then(non_empty_str)
        .or_else(|| config_url.and_then(non_empty_str))
        .map(normalize_base_url)
        .transpose()?
        .unwrap_or_else(|| DEFAULT_BEATBOX_URL.to_string());
    let api_key = env_api_key
        .and_then(non_empty_str)
        .or_else(|| config_api_key.and_then(non_empty_str))
        .map(str::to_string);
    Ok(BeatboxConfig { url, api_key })
}

fn non_empty_str(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

fn resolve_public_base_url(
    host: std::net::IpAddr,
    port: u16,
    base_url_override: Option<&str>,
    env_base_url: Option<&str>,
    config_base_url: Option<&str>,
) -> Result<String> {
    let raw = base_url_override
        .and_then(non_empty_str)
        .or_else(|| env_base_url.and_then(non_empty_str))
        .or(config_base_url);
    match raw {
        Some(value) => normalize_base_url(value),
        None if host_is_unspecified(host) => bail!(
            "binding {host} requires an explicit public base URL; set --base-url, {BASE_URL_ENV}, or [app].base_url"
        ),
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

fn host_is_unspecified(host: std::net::IpAddr) -> bool {
    match host {
        std::net::IpAddr::V4(host) => host.is_unspecified(),
        std::net::IpAddr::V6(host) => host.is_unspecified(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BASE_URL_ENV, default_base_url, normalize_base_url, resolve_beatbox_config_with_env,
        resolve_public_base_url,
    };
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

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

    #[test]
    fn public_base_url_requires_explicit_url_for_unspecified_hosts() {
        for host in [
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        ] {
            let error = resolve_public_base_url(host, 3000, None, None, None).unwrap_err();
            let message = error.to_string();
            assert!(message.contains("--base-url"), "{message}");
            assert!(message.contains(BASE_URL_ENV), "{message}");
            assert!(message.contains("[app].base_url"), "{message}");

            let resolved =
                resolve_public_base_url(host, 3000, Some("https://public.example"), None, None)
                    .unwrap();
            assert_eq!(resolved, "https://public.example");
        }
    }

    #[test]
    fn public_base_url_ignores_empty_override_and_env_values() {
        let host = "127.0.0.1".parse().unwrap();

        let resolved = resolve_public_base_url(
            host,
            3000,
            Some(" "),
            Some(""),
            Some("https://config.example"),
        )
        .unwrap();
        assert_eq!(resolved, "https://config.example");

        let resolved = resolve_public_base_url(
            host,
            3000,
            Some(""),
            Some(" https://env.example/// "),
            Some("https://config.example"),
        )
        .unwrap();
        assert_eq!(resolved, "https://env.example");

        let resolved = resolve_public_base_url(host, 3000, None, Some(" "), None).unwrap();
        assert_eq!(resolved, "http://127.0.0.1:3000");
    }

    #[test]
    fn beatbox_config_prefers_env_then_config_then_loopback_default() {
        let resolved = resolve_beatbox_config_with_env(
            Some("http://127.0.0.1:7300/"),
            Some("config-token"),
            Some("http://127.0.0.1:7400///"),
            Some("env-token"),
        )
        .unwrap();
        assert_eq!(resolved.url, "http://127.0.0.1:7400");
        assert_eq!(resolved.api_key.as_deref(), Some("env-token"));

        let resolved = resolve_beatbox_config_with_env(
            Some("http://127.0.0.1:7300"),
            Some("config-token"),
            None,
            None,
        )
        .unwrap();
        assert_eq!(resolved.url, "http://127.0.0.1:7300");
        assert_eq!(resolved.api_key.as_deref(), Some("config-token"));

        let resolved = resolve_beatbox_config_with_env(None, None, None, None).unwrap();
        assert_eq!(resolved.url, beater_agent::DEFAULT_BEATBOX_URL);
        assert!(resolved.api_key.is_none());
    }

    #[test]
    fn beatbox_url_reuses_base_url_validation() {
        assert!(
            resolve_beatbox_config_with_env(Some("file:///tmp/socket"), None, None, None).is_err()
        );
        assert!(
            resolve_beatbox_config_with_env(
                Some("https://user:pass@example.com"),
                None,
                None,
                None
            )
            .is_err()
        );
    }
}
