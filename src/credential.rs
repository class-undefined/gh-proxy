//! Client-side Git credential helper. Credentials never become gateway configuration.
use anyhow::{Result, ensure};
use clap::ValueEnum;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};
use url::Url;
const LIMIT: u64 = 65536;
#[derive(Clone, Copy, ValueEnum)]
pub enum Operation {
    Get,
    Store,
    Erase,
}
fn fields(input: &str) -> Result<BTreeMap<String, String>> {
    ensure!(
        input.len() <= LIMIT as usize && !input.contains('\0'),
        "invalid credential request"
    );
    let mut values = BTreeMap::new();
    for line in input.lines().take_while(|l| !l.is_empty()) {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if matches!(key, "protocol" | "host" | "path" | "username" | "password") {
            ensure!(
                !values.contains_key(key) && !value.chars().any(char::is_control),
                "invalid credential fields"
            );
            values.insert(key.into(), value.into());
        }
    }
    Ok(values)
}
fn matches_gateway(values: &BTreeMap<String, String>, gateway: &Url) -> bool {
    let (Some(protocol), Some(host)) = (values.get("protocol"), values.get("host")) else {
        return false;
    };
    let Ok(candidate) = Url::parse(&format!("{protocol}://{host}")) else {
        return false;
    };
    candidate.username().is_empty()
        && candidate.password().is_none()
        && candidate.path() == "/"
        && candidate.query().is_none()
        && candidate.fragment().is_none()
        && candidate.origin() == gateway.origin()
}
fn upstream_context(values: &BTreeMap<String, String>, operation: Operation) -> String {
    let mut output = "protocol=https\nhost=github.com\n".to_string();
    for key in ["path", "username", "password"] {
        if key == "password" && matches!(operation, Operation::Get) {
            continue;
        }
        if let Some(value) = values.get(key) {
            output.push_str(&format!("{key}={value}\n"));
        }
    }
    output.push('\n');
    output
}
fn credentials(output: &[u8], username: Option<&String>) -> Option<String> {
    let input = std::str::from_utf8(output).ok()?;
    let values = fields(input).ok()?;
    let user = values.get("username").filter(|v| !v.is_empty())?;
    let password = values.get("password").filter(|v| !v.is_empty())?;
    if username.is_some_and(|expected| !expected.is_empty() && expected != user) {
        return None;
    }
    Some(format!("username={user}\npassword={password}\n\n"))
}
async fn provider(program: &str, args: &[&str], input: &str) -> Option<Vec<u8>> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_ASKPASS", "")
        .env("SSH_ASKPASS", "")
        .env_remove("GIT_TRACE")
        .env_remove("GIT_TRACE_CURL")
        .env_remove("GH_DEBUG");
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut child = command.spawn().ok()?;
        let mut stdin = child.stdin.take()?;
        let mut stdout = child.stdout.take()?.take(LIMIT + 1);
        let mut output = Vec::new();
        let (written, read) = tokio::join!(
            async {
                let result = stdin.write_all(input.as_bytes()).await;
                drop(stdin);
                result
            },
            stdout.read_to_end(&mut output)
        );
        written.ok()?;
        read.ok()?;
        if output.len() > LIMIT as usize {
            return None;
        }
        child.wait().await.ok()?.success().then_some(output)
    })
    .await
    .ok()
    .flatten()
}
pub async fn run(gateway: Url, operation: Operation) -> Result<()> {
    let local = gateway.host_str().is_some_and(|h| {
        h == "localhost"
            || h.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    ensure!(
        (gateway.scheme() == "https" || (gateway.scheme() == "http" && local))
            && gateway.path() == "/"
            && gateway.query().is_none()
            && gateway.fragment().is_none()
            && gateway.username().is_empty()
            && gateway.password().is_none(),
        "gateway must be an HTTPS origin (HTTP allowed only for loopback development)"
    );
    let mut input = String::new();
    std::io::stdin()
        .take(LIMIT + 1)
        .read_to_string(&mut input)?;
    let values = fields(&input)?;
    if !matches_gateway(&values, &gateway) {
        return Ok(());
    }
    let mapped = upstream_context(&values, operation);
    match operation {
        Operation::Get => {
            let mut found = None;
            if let Some(output) = provider("gh", &["auth", "git-credential", "get"], &mapped).await
            {
                found = credentials(&output, values.get("username"));
            }
            if found.is_none()
                && let Some(output) = provider(
                    "git",
                    &[
                        "-c",
                        "credential.interactive=false",
                        "-c",
                        "core.askPass=",
                        "credential",
                        "fill",
                    ],
                    &mapped,
                )
                .await
            {
                found = credentials(&output, values.get("username"));
            }
            if let Some(found) = found {
                std::io::stdout().write_all(found.as_bytes())?;
            }
        }
        Operation::Store | Operation::Erase => {
            if values.get("username").is_some_and(|v| !v.is_empty()) {
                let action = if matches!(operation, Operation::Store) {
                    "approve"
                } else {
                    "reject"
                };
                let _ = provider("git", &["credential", action], &mapped).await;
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_only_the_explicit_gateway_and_uses_github_https() {
        let gateway = Url::parse("https://gateway.example").unwrap();
        let input =
            fields("protocol=https\nhost=gateway.example\npath=owner/repo.git\nusername=alice\n\n")
                .unwrap();
        assert!(matches_gateway(&input, &gateway));
        let mapped = upstream_context(&input, Operation::Get);
        assert!(mapped.contains("host=github.com\n"));
        assert!(mapped.contains("path=owner/repo.git\n"));
        for host in [
            "evil.example",
            "github.com",
            "user@gateway.example",
            "gateway.example/path",
        ] {
            let v = fields(&format!("protocol=https\nhost={host}\n\n")).unwrap();
            assert!(!matches_gateway(&v, &gateway));
        }
    }
    #[test]
    fn strips_provider_context_and_respects_the_selected_account() {
        let result = credentials(
            b"protocol=https\nhost=github.com\nusername=alice\npassword=fixture-token\n\n",
            None,
        )
        .unwrap();
        assert!(!result.contains("host="));
        assert!(result.starts_with("username=alice\n"));
        assert!(
            credentials(
                b"username=bob\npassword=fixture-token\n\n",
                Some(&"alice".into())
            )
            .is_none()
        );
        assert!(fields("host=one\nhost=two\n\n").is_err());
        assert!(credentials(b"username=alice\npassword=\n\n", None).is_none());
    }
}
