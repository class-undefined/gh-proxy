//! A real Git client completes a Smart HTTP 401 challenge through our client helper.
#![cfg(unix)]
use std::{
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn git_reuses_github_credentials_only_for_the_configured_gateway() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let bin = root.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let provider = bin.join("gh");
    std::fs::write(
        &provider,
        r#"#!/bin/sh
input="$(cat)"
case "$input" in *"protocol=https"*"host=github.com"*) ;; *) exit 1;; esac
printf 'username=fixture\npassword=token\n\n'
"#,
    )
    .unwrap();
    std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o700)).unwrap();
    let sandbox = |cmd: &mut Command| {
        cmd.env("HOME", root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .env_remove("GIT_CONFIG_PARAMETERS")
            .env_remove("GIT_CONFIG_COUNT");
    };
    let repository = root.join("repo.git");
    let mut init = Command::new("git");
    sandbox(&mut init);
    assert!(
        init.args(["init", "--bare", "--quiet", "--template="])
            .arg(&repository)
            .status()
            .unwrap()
            .success()
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let served_root = root.to_path_buf();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(25);
        let mut challenged = false;
        loop {
            assert!(
                Instant::now() < deadline,
                "Git did not complete the authentication exchange"
            );
            let (mut stream, _) = match listener.accept() {
                Ok(pair) => pair,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(e) => panic!("fixture listener failed: {e}"),
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                head.push(byte[0]);
                assert!(head.len() < 16384);
            }
            let headers = String::from_utf8(head).unwrap().to_ascii_lowercase();
            if !headers.contains("authorization: basic zml4dhvyztp0b2tlbg==\r\n") {
                challenged = true;
                stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"GitHub\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                continue;
            }
            assert!(
                challenged,
                "Git must first receive the credential challenge"
            );
            let result = Command::new("git")
                .args(["-c", "protocol.version=0", "http-backend"])
                .env("GIT_PROJECT_ROOT", served_root)
                .env("GIT_HTTP_EXPORT_ALL", "1")
                .env("REQUEST_METHOD", "GET")
                .env("PATH_INFO", "/repo.git/info/refs")
                .env("QUERY_STRING", "service=git-upload-pack")
                .output()
                .unwrap();
            assert!(result.status.success());
            let split = result
                .stdout
                .windows(4)
                .position(|s| s == b"\r\n\r\n")
                .unwrap();
            let body = &result.stdout[split + 4..];
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/x-git-upload-pack-advertisement\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
            stream.write_all(body).unwrap();
            break;
        }
    });
    let binary = env!("CARGO_BIN_EXE_gh-proxy");
    let mut git = Command::new("git");
    sandbox(&mut git);
    let result = git
        .args([
            "-c",
            "credential.helper=",
            "-c",
            "http.proxy=",
            "-c",
            "protocol.version=0",
            "-c",
        ])
        .arg(format!(
            "credential.{origin}.helper=!'{binary}' credential --gateway '{origin}'"
        ))
        .args(["ls-remote", &format!("{origin}/owner/repo.git")])
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(
        result.status.success(),
        "Git failed to authenticate through the scoped helper"
    );
    assert!(
        result.stdout.is_empty(),
        "fixture repository should be empty"
    );

    // Even if registered as a general helper, a different origin cannot retrieve credentials.
    let mut helper = Command::new(binary);
    sandbox(&mut helper);
    let mut child = helper
        .args(["credential", "--gateway", &origin, "get"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"protocol=https\nhost=evil.example\n\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
}
