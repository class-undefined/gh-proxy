use anyhow::{Context, Result, ensure};
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    net::{IpAddr, SocketAddr},
    path::Path,
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub http_listen: SocketAddr,
    pub socks_listen: SocketAddr,
    pub public_url: url::Url,
    pub clients: Vec<IpNet>,
    #[serde(default)]
    pub direct: bool,
    #[serde(default = "default_web_enabled")]
    pub web_enabled: bool,
    #[serde(default = "default_connections")]
    pub max_connections: usize,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    pub tls: Option<Tls>,
    #[serde(default)]
    pub upstream: Upstream,
    #[serde(default)]
    pub repositories: Vec<Repository>,
    #[serde(default)]
    pub ssh_targets: Vec<SshTarget>,
}
fn default_web_enabled() -> bool {
    true
}
fn default_connections() -> usize {
    128
}
fn default_timeout() -> u64 {
    300
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Tls {
    pub cert: String,
    pub key: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Repository {
    pub name: String,
    #[serde(default)]
    pub write: bool,
    #[serde(default)]
    pub release: Release,
}
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Release {
    #[serde(default)]
    pub query: bool,
    #[serde(default)]
    pub create: bool,
    #[serde(default)]
    pub update: bool,
    #[serde(default)]
    pub upload: bool,
    #[serde(default)]
    pub download: bool,
    #[serde(default)]
    pub delete: bool,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SshTarget {
    pub host: String,
    pub port: u16,
}

pub fn component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}
impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let input = std::fs::read_to_string(path).context("cannot read configuration")?;
        // TOML errors include source snippets, which may contain secrets.
        let config: Self = toml::from_str(&input)
            .map_err(|_| anyhow::anyhow!("invalid configuration syntax or fields"))?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.clients.is_empty(),
            "client allowlist must not be empty"
        );
        ensure!(
            self.max_connections > 0 && self.max_connections <= 65536,
            "invalid connection limit"
        );
        ensure!(
            (1..=86400).contains(&self.timeout_seconds),
            "timeout must be between 1 and 86400 seconds"
        );
        ensure!(
            self.http_listen != self.socks_listen,
            "listeners must differ"
        );
        ensure!(
            self.public_url.username().is_empty()
                && self.public_url.password().is_none()
                && self.public_url.query().is_none()
                && self.public_url.fragment().is_none()
                && self.public_url.path() == "/",
            "public_url must be an origin without credentials"
        );
        let local_http = self.http_listen.ip().is_loopback()
            && self.public_url.scheme() == "http"
            && self.public_url.host_str().is_some_and(|h| {
                h == "localhost" || h.parse::<IpAddr>().is_ok_and(|i| i.is_loopback())
            });
        ensure!(
            self.public_url.scheme() == "https" || local_http,
            "public_url requires HTTPS except loopback development"
        );
        ensure!(
            self.tls.is_some() || self.http_listen.ip().is_loopback(),
            "plaintext HTTP must bind loopback behind a TLS reverse proxy"
        );
        ensure!(
            self.tls.is_none() || self.public_url.scheme() == "https",
            "TLS listener requires an HTTPS public_url"
        );
        let mut names = HashSet::new();
        for repo in &self.repositories {
            let parts: Vec<_> = repo.name.split('/').collect();
            ensure!(
                parts.len() == 2
                    && parts.iter().all(|p| component(p))
                    && !parts[1].ends_with(".git"),
                "repository must be owner/repo without .git"
            );
            ensure!(
                names.insert(repo.name.to_ascii_lowercase()),
                "duplicate repository policy"
            );
        }
        for target in &self.ssh_targets {
            ensure!(
                matches!(
                    (target.host.as_str(), target.port),
                    ("github.com", 22) | ("ssh.github.com", 443)
                ),
                "unsupported SSH platform target"
            );
        }
        Ok(())
    }
    pub fn allows_client(&self, ip: IpAddr) -> bool {
        let ip = match ip {
            IpAddr::V6(v) => v.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
            _ => ip,
        };
        self.clients.iter().any(|net| net.contains(&ip))
    }
    pub fn repository(&self, name: &str) -> Option<&Repository> {
        self.repositories
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn example_is_valid_and_deny_by_default() {
        let c: Config = toml::from_str(include_str!("../tests/fixtures/gateway.toml")).unwrap();
        c.validate().unwrap();
        assert!(!c.direct);
        assert!(c.allows_client("127.0.0.1".parse().unwrap()));
        assert!(!c.allows_client("192.168.1.2".parse().unwrap()));
        assert!(!c.repositories[0].write);
    }
    #[test]
    fn unsafe_config_rejected() {
        let mut c: Config = toml::from_str(include_str!("../tests/fixtures/gateway.toml")).unwrap();
        c.http_listen = "0.0.0.0:8080".parse().unwrap();
        assert!(c.validate().is_err());
        c.http_listen = "127.0.0.1:8080".parse().unwrap();
        c.repositories[0].name = "owner/../repo".into();
        assert!(c.validate().is_err());
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Upstream {
    pub http_proxy: Option<String>,
    pub https_proxy: Option<String>,
    pub all_proxy: Option<String>,
    pub no_proxy: Option<String>,
}
