use crate::config::{Config, component};
use anyhow::{Result, bail, ensure};
use axum::http::{Method, Uri};
use url::Url;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Read,
    Write,
    Query,
    Create,
    Update,
    Upload,
    Download,
    Delete,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Git,
    Api,
    Upload,
    Download,
}
#[derive(Clone, Debug)]
pub struct Route {
    pub repository: String,
    pub operation: Operation,
    pub kind: Kind,
    pub target: Url,
}

fn segment(raw: &str) -> Result<String> {
    let decoded = percent_encoding::percent_decode_str(raw)
        .decode_utf8()
        .map_err(|_| anyhow::anyhow!("invalid path encoding"))?;
    ensure!(
        !decoded.is_empty()
            && decoded != "."
            && decoded != ".."
            && !decoded
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '\\' | '%' | '?' | '#')),
        "ambiguous path"
    );
    Ok(decoded.into())
}
fn id(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit())
}
fn release_operation(
    method: &Method,
    tail: &[String],
    binary: bool,
    upload: bool,
) -> Result<Operation> {
    use Operation::*;
    let p: Vec<_> = tail.iter().map(String::as_str).collect();
    if upload {
        ensure!(
            *method == Method::POST && matches!(p.as_slice(), ["releases", n, "assets"] if id(n)),
            "unknown upload operation"
        );
        return Ok(Upload);
    }
    let op = match (method.as_str(), p.as_slice()) {
        ("GET" | "HEAD", ["releases"])
        | ("GET" | "HEAD", ["releases", "latest"])
        | ("GET" | "HEAD", ["releases", "tags", _]) => Query,
        ("GET" | "HEAD", ["releases", n]) if id(n) => Query,
        ("GET" | "HEAD", ["releases", n, "assets"]) if id(n) => Query,
        ("GET" | "HEAD", ["releases", "assets", n]) if id(n) => {
            if binary {
                Download
            } else {
                Query
            }
        }
        ("POST", ["releases"]) | ("POST", ["releases", "generate-notes"]) => Create,
        ("PATCH", ["releases", n]) if id(n) => Update,
        ("PATCH", ["releases", "assets", n]) if id(n) => Update,
        ("DELETE", ["releases", n]) if id(n) => Delete,
        ("DELETE", ["releases", "assets", n]) if id(n) => Delete,
        _ => bail!("unknown API operation"),
    };
    Ok(op)
}
fn authorize(config: &Config, route: &Route) -> Result<()> {
    let r = config
        .repository(&route.repository)
        .ok_or_else(|| anyhow::anyhow!("repository denied"))?;
    let allowed = match route.operation {
        Operation::Read => true,
        Operation::Write => r.write,
        Operation::Query => r.release.query,
        Operation::Create => r.release.create,
        Operation::Update => r.release.update,
        Operation::Upload => r.release.upload,
        Operation::Download => r.release.download,
        Operation::Delete => r.release.delete,
    };
    ensure!(allowed, "operation denied");
    Ok(())
}

/// GitHub-specific route adapter. Add other platforms as separate adapters, never arbitrary origins.
pub fn github(config: &Config, method: &Method, uri: &Uri, binary: bool) -> Result<Route> {
    ensure!(
        uri.scheme().is_none() && uri.authority().is_none(),
        "absolute request target denied"
    );
    let path = uri.path();
    ensure!(
        path.starts_with('/') && !path.contains("//") && path.len() < 8192,
        "invalid path"
    );
    let parts: Vec<String> = path[1..].split('/').map(segment).collect::<Result<_>>()?;
    let (kind, start, origin) = if path.starts_with("/api/github/") {
        (Kind::Api, 3, "https://api.github.com")
    } else if path.starts_with("/uploads/github/") {
        (Kind::Upload, 3, "https://uploads.github.com")
    } else {
        (Kind::Git, 0, "https://github.com")
    };
    let repo_parts = parts
        .get(start..start + 2)
        .ok_or_else(|| anyhow::anyhow!("missing repository"))?;
    if start > 0 {
        ensure!(parts[2] == "repos", "unknown API namespace");
    }
    let repo = if kind == Kind::Git {
        repo_parts[1].strip_suffix(".git").unwrap_or(&repo_parts[1])
    } else {
        &repo_parts[1]
    };
    ensure!(
        component(&repo_parts[0]) && component(repo),
        "invalid repository"
    );
    let repository = format!("{}/{}", repo_parts[0], repo).to_ascii_lowercase();
    let tail = &parts[start + 2..];
    let query: Vec<(String, String)> =
        url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    let (operation, kind) = match kind {
        Kind::Git => {
            if matches!(tail, [a,b,_,_] if a == "releases" && b == "download") {
                ensure!(
                    matches!(method.as_str(), "GET" | "HEAD") && query.is_empty(),
                    "invalid download request"
                );
                (Operation::Download, Kind::Download)
            } else {
                let p: Vec<_> = tail.iter().map(String::as_str).collect();
                let service = match (method.as_str(), p.as_slice()) {
                    ("GET", ["info", "refs"]) => {
                        ensure!(
                            query.len() == 1 && query[0].0 == "service",
                            "service required"
                        );
                        query[0].1.as_str()
                    }
                    ("POST", [s @ ("git-upload-pack" | "git-receive-pack")]) => {
                        ensure!(query.is_empty(), "unexpected query");
                        s
                    }
                    _ => bail!("unknown Git operation"),
                };
                (
                    match service {
                        "git-upload-pack" => Operation::Read,
                        "git-receive-pack" => Operation::Write,
                        _ => bail!("unknown Git service"),
                    },
                    Kind::Git,
                )
            }
        }
        Kind::Api | Kind::Upload => {
            let mut seen = std::collections::HashSet::new();
            for (key, value) in &query {
                ensure!(seen.insert(key) && value.len() <= 1024, "invalid query");
                ensure!(
                    if kind == Kind::Upload {
                        matches!(key.as_str(), "name" | "label")
                    } else {
                        matches!(key.as_str(), "page" | "per_page")
                            && id(value)
                            && matches!(method.as_str(), "GET" | "HEAD")
                    },
                    "query denied"
                );
            }
            if kind == Kind::Upload {
                ensure!(
                    query.iter().any(|(k, v)| k == "name" && !v.is_empty()),
                    "upload name required"
                );
            }
            (
                release_operation(method, tail, binary, kind == Kind::Upload)?,
                kind,
            )
        }
        Kind::Download => unreachable!(),
    };
    let mut target = Url::parse(origin)?;
    let raw_path = if start > 0 {
        path.splitn(4, '/')
            .nth(3)
            .ok_or_else(|| anyhow::anyhow!("invalid API path"))?
    } else {
        path
    };
    target.set_path(raw_path);
    target.set_query(uri.query());
    let route = Route {
        repository,
        operation,
        kind,
        target,
    };
    authorize(config, &route)?;
    Ok(route)
}

pub fn redirect_target(route: &Route, previous: &Url, location: &str) -> Result<Url> {
    ensure!(
        route.operation == Operation::Download,
        "redirect denied for this operation"
    );
    let url = previous
        .join(location)
        .map_err(|_| anyhow::anyhow!("invalid redirect"))?;
    ensure!(
        url.scheme() == "https"
            && url.port_or_known_default() == Some(443)
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none(),
        "unsafe redirect"
    );
    match url.host_str() {
        Some("release-assets.githubusercontent.com" | "objects.githubusercontent.com") => {
            ensure!(
                url.path().starts_with("/github-production-release-asset/"),
                "unknown attachment path"
            );
        }
        Some("github.com") => {
            let prefix = format!("/{}/releases/download/", route.repository);
            ensure!(
                url.path().to_ascii_lowercase().starts_with(&prefix),
                "cross-repository redirect denied"
            );
        }
        _ => bail!("redirect target denied"),
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> Config {
        toml::from_str(include_str!("../config/example.toml")).unwrap()
    }
    fn check(method: &str, path: &str, binary: bool) -> Result<Route> {
        github(
            &config(),
            &method.parse().unwrap(),
            &path.parse().unwrap(),
            binary,
        )
    }
    #[test]
    fn git_read_write_and_discovery() {
        assert_eq!(
            check(
                "GET",
                "/class-undefined/gh-proxy.git/info/refs?service=git-upload-pack",
                false
            )
            .unwrap()
            .operation,
            Operation::Read
        );
        assert!(
            check(
                "POST",
                "/class-undefined/gh-proxy.git/git-upload-pack",
                false
            )
            .is_ok()
        );
        for (m, p) in [
            (
                "GET",
                "/class-undefined/gh-proxy.git/info/refs?service=git-receive-pack",
            ),
            ("POST", "/class-undefined/gh-proxy.git/git-receive-pack"),
            ("GET", "/other/repo.git/info/refs?service=git-upload-pack"),
        ] {
            assert!(check(m, p, false).is_err());
        }
    }
    #[test]
    fn path_and_query_confusion_rejected() {
        for path in [
            "/class-undefined/%2e%2e/info/refs?service=git-upload-pack",
            "/class-undefined/gh-proxy.git//git-upload-pack",
            "/class-undefined/gh-proxy.git/info/refs?service=git-upload-pack&service=git-receive-pack",
            "/api/github/repos/class-undefined/gh-proxy/contents/a",
            "/api/github/repos/class-undefined/gh-proxy/releases?url=https://evil.com",
        ] {
            assert!(check("GET", path, false).is_err(), "{path}");
        }
    }
    #[test]
    fn release_operations_and_origins() {
        let r = check(
            "GET",
            "/api/github/repos/class-undefined/gh-proxy/releases",
            false,
        )
        .unwrap();
        assert_eq!(
            r.target.as_str(),
            "https://api.github.com/repos/class-undefined/gh-proxy/releases"
        );
        assert!(
            check(
                "GET",
                "/api/github/repos/class-undefined/gh-proxy/releases/assets/1",
                true
            )
            .is_ok()
        );
        assert!(
            check(
                "POST",
                "/api/github/repos/class-undefined/gh-proxy/releases",
                false
            )
            .is_err()
        );
        assert!(
            check(
                "PATCH",
                "/api/github/repos/class-undefined/gh-proxy/releases/1",
                false
            )
            .is_err()
        );
        assert!(
            check(
                "DELETE",
                "/api/github/repos/class-undefined/gh-proxy/releases/1",
                false
            )
            .is_err()
        );
        let mut c = config();
        c.repositories[0].release.upload = true;
        let r = github(
            &c,
            &Method::POST,
            &"/uploads/github/repos/class-undefined/gh-proxy/releases/1/assets?name=a.zip"
                .parse()
                .unwrap(),
            false,
        )
        .unwrap();
        assert_eq!(r.target.host_str(), Some("uploads.github.com"));
        assert_eq!(r.operation, Operation::Upload);
    }
    #[test]
    fn redirects_keep_repository_scope() {
        let r = check(
            "GET",
            "/api/github/repos/class-undefined/gh-proxy/releases/assets/1",
            true,
        )
        .unwrap();
        assert!(redirect_target(&r,&r.target,"https://release-assets.githubusercontent.com/github-production-release-asset/123/file?sig=secret").is_ok());
        for target in [
            "http://github.com/a",
            "https://127.0.0.1/a",
            "https://github.com/other/repo/releases/download/v1/a",
            "https://evil.githubusercontent.com/a",
            "https://objects.githubusercontent.com/arbitrary",
        ] {
            assert!(redirect_target(&r, &r.target, target).is_err());
        }
    }
}
