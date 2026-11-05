use anyhow::{Result, bail, ensure};
use reqwest::{
    Client, Proxy,
    dns::{Addrs, Name, Resolve, Resolving},
};
use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use url::Url;

// Never derive Debug: proxy URLs can contain passwords.
#[derive(Clone)]
pub struct Egress {
    pub(crate) all: Option<Url>,
    http: Option<Url>,
    https: Option<Url>,
    no_proxy: Vec<Bypass>,
    pub(crate) direct: bool,
    pub(crate) timeout: Duration,
}
#[derive(Clone)]
struct Bypass {
    host: String,
    port: Option<u16>,
}

fn variable(get: &impl Fn(&str) -> Option<String>, upper: &str) -> Option<String> {
    get(upper)
        .or_else(|| get(&upper.to_ascii_lowercase()))
        .filter(|s| !s.is_empty())
}
fn proxy(value: Option<String>, socks_only: bool) -> Result<Option<Url>> {
    value
        .map(|s| {
            let url = Url::parse(&s).map_err(|_| anyhow::anyhow!("invalid proxy URL"))?;
            let allowed = if socks_only {
                matches!(url.scheme(), "socks5" | "socks5h")
            } else {
                matches!(url.scheme(), "http" | "https" | "socks5" | "socks5h")
            };
            ensure!(
                allowed
                    && url.host_str().is_some()
                    && url.port_or_known_default().is_some()
                    && matches!(url.path(), "" | "/")
                    && url.query().is_none()
                    && url.fragment().is_none(),
                "unsupported proxy URL"
            );
            Ok(url)
        })
        .transpose()
}
impl Egress {
    pub fn from_env(direct: bool, timeout: Duration) -> Result<Self> {
        Self::from_lookup(|s| std::env::var(s).ok(), direct, timeout)
    }
    pub fn from_lookup(
        get: impl Fn(&str) -> Option<String>,
        direct: bool,
        timeout: Duration,
    ) -> Result<Self> {
        let mut no_proxy = Vec::new();
        if let Some(value) = variable(&get, "NO_PROXY") {
            for rule in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                ensure!(
                    !rule.contains('/') && !rule.contains('@'),
                    "NO_PROXY CIDR and URL syntax are not supported"
                );
                let parsed = if rule == "*" {
                    Bypass {
                        host: "*".into(),
                        port: None,
                    }
                } else if let Ok(ip) = rule.parse::<IpAddr>() {
                    Bypass {
                        host: ip.to_string(),
                        port: None,
                    }
                } else {
                    let stripped = rule.trim_start_matches('.');
                    let u = Url::parse(&format!("http://{stripped}"))
                        .map_err(|_| anyhow::anyhow!("invalid NO_PROXY rule"))?;
                    ensure!(
                        u.path() == "/"
                            && u.query().is_none()
                            && u.fragment().is_none()
                            && u.username().is_empty(),
                        "invalid NO_PROXY rule"
                    );
                    Bypass {
                        host: u
                            .host_str()
                            .ok_or_else(|| anyhow::anyhow!("invalid NO_PROXY host"))?
                            .trim_matches(['[', ']'])
                            .to_ascii_lowercase(),
                        port: u.port(),
                    }
                };
                no_proxy.push(parsed);
            }
        }
        Ok(Self {
            http: proxy(variable(&get, "HTTP_PROXY"), false)?,
            https: proxy(variable(&get, "HTTPS_PROXY"), false)?,
            all: proxy(variable(&get, "ALL_PROXY"), true)?,
            no_proxy,
            direct,
            timeout,
        })
    }
    fn bypass(&self, url: &Url) -> bool {
        let host = url
            .host_str()
            .unwrap_or("")
            .trim_matches(['[', ']'])
            .to_ascii_lowercase();
        self.no_proxy.iter().any(|r| {
            (r.host == "*" || host == r.host || host.ends_with(&format!(".{}", r.host)))
                && r.port
                    .is_none_or(|p| Some(p) == url.port_or_known_default())
        })
    }
    pub fn client(&self, url: &Url) -> Result<Client> {
        let selected = if self.bypass(url) {
            None
        } else {
            match url.scheme() {
                "http" => self.http.as_ref(),
                "https" => self.https.as_ref(),
                _ => bail!("unsupported target protocol"),
            }
        };
        ensure!(selected.is_some() || self.direct, "direct egress denied");
        let mut builder = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .timeout(self.timeout)
            .dns_resolver(Arc::new(PublicResolver))
            .user_agent("gh-proxy/0.1");
        if let Some(proxy) = selected {
            // Explicit proxy overrides all library/environment inference; no direct fallback exists.
            builder = builder.proxy(
                Proxy::all(proxy.as_str())
                    .map_err(|_| anyhow::anyhow!("invalid HTTP proxy configuration"))?,
            );
            // Upstream proxy may be on loopback/private networks. Its DNS is trusted.
            builder = builder.dns_resolver(Arc::new(SystemResolver));
        }
        builder
            .build()
            .map_err(|_| anyhow::anyhow!("cannot initialize HTTP transport"))
    }
    pub fn ssh_available(&self) -> bool {
        self.all.is_some() || self.direct
    }
    pub fn http_available(&self) -> bool {
        self.https.is_some() || self.direct
    }
}

pub fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            !(v.is_private()
                || v.is_loopback()
                || v.is_link_local()
                || v.is_unspecified()
                || v.is_multicast()
                || v.is_broadcast()
                || v.is_documentation()
                || v.octets()[0] == 0
                || v.octets()[0] >= 240
                || (v.octets()[0] == 100 && (64..=127).contains(&v.octets()[1]))
                || (v.octets()[0] == 198 && matches!(v.octets()[1], 18 | 19)))
        }
        IpAddr::V6(v) => {
            let s = v.segments();
            // Only global unicast; exclude documentation and IPv4 transition mechanisms.
            (s[0] & 0xe000) == 0x2000
                && s[0] != 0x2002
                && !(s[0] == 0x2001 && (s[1] < 0x0200 || s[1] == 0x0db8))
        }
    }
}
struct PublicResolver;
struct SystemResolver;
async fn resolve(
    name: Name,
    public_only: bool,
) -> Result<Addrs, Box<dyn std::error::Error + Send + Sync>> {
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((name.as_str(), 0)).await?.collect();
    if addrs.is_empty() || (public_only && addrs.iter().any(|a| !public_ip(a.ip()))) {
        return Err("target DNS rejected".into());
    }
    Ok(Box::new(addrs.into_iter()))
}
impl Resolve for PublicResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(resolve(name, true))
    }
}
impl Resolve for SystemResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(resolve(name, false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn env(pairs: &[(&str, &str)], direct: bool) -> Egress {
        Egress::from_lookup(
            |k| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == k)
                    .map(|(_, v)| v.to_string())
            },
            direct,
            Duration::from_secs(1),
        )
        .unwrap()
    }
    #[test]
    fn uppercase_empty_and_protocol_separation() {
        let e = env(
            &[
                ("HTTPS_PROXY", ""),
                ("https_proxy", "http://localhost:9"),
                ("ALL_PROXY", "socks5h://localhost:9"),
            ],
            false,
        );
        assert!(
            e.client(&Url::parse("https://github.com").unwrap())
                .is_err()
        );
        assert!(e.ssh_available());
        let e = env(&[("HTTP_PROXY", "http://localhost:9")], false);
        assert!(
            e.client(&Url::parse("https://github.com").unwrap())
                .is_err()
        );
    }
    #[test]
    fn bypass_boundaries_and_deny_mode() {
        let e = env(
            &[
                ("HTTPS_PROXY", "http://localhost:9"),
                ("NO_PROXY", ".github.com:443"),
            ],
            false,
        );
        assert!(e.bypass(&Url::parse("https://api.github.com").unwrap()));
        assert!(!e.bypass(&Url::parse("https://evil-github.com").unwrap()));
        assert!(!e.bypass(&Url::parse("https://github.com:444").unwrap()));
        assert!(
            e.client(&Url::parse("https://github.com").unwrap())
                .is_err()
        );
    }
    #[test]
    fn secrets_not_in_errors_and_private_ips_rejected() {
        let err = Egress::from_lookup(
            |k| (k == "ALL_PROXY").then(|| "http://user:secret@localhost:9".into()),
            false,
            Duration::from_secs(1),
        )
        .err()
        .unwrap();
        assert!(!err.to_string().contains("secret"));
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.169.254",
            "::1",
            "::ffff:127.0.0.1",
            "2001:db8::1",
        ] {
            assert!(!public_ip(ip.parse().unwrap()));
        }
    }
    #[tokio::test]
    async fn dead_proxy_never_falls_back() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let e = env(&[("HTTPS_PROXY", &format!("http://{addr}"))], true);
        let url = Url::parse("https://github.com").unwrap();
        assert!(e.client(&url).unwrap().get(url).send().await.is_err());
    }
}
