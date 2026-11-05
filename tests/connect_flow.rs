//! Exercise the actual ProxyCommand process, including stdin EOF and remote EOF.
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    process::Command,
    time::timeout,
};

fn command(address: std::net::SocketAddr) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_gh-proxy"));
    cmd.args([
        "connect",
        "--socks",
        &address.to_string(),
        "unresolvable.invalid",
        "22",
    ])
    // An unusable environment must not change the explicitly selected gateway.
    .env("ALL_PROXY", "socks5h://127.0.0.1:1")
    .env("HTTPS_PROXY", "http://127.0.0.1:1")
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    cmd
}
async fn request(socket: &mut TcpStream) {
    let mut greeting = [0; 3];
    socket.read_exact(&mut greeting).await.unwrap();
    assert_eq!(greeting, [5, 1, 0]);
    socket.write_all(&[5, 0]).await.unwrap();
    let mut head = [0; 5];
    socket.read_exact(&mut head).await.unwrap();
    assert_eq!(head, [5, 1, 0, 3, 20]);
    let mut host = [0; 20];
    socket.read_exact(&mut host).await.unwrap();
    assert_eq!(&host, b"unresolvable.invalid");
    assert_eq!(socket.read_u16().await.unwrap(), 22);
}

#[tokio::test]
async fn binary_duplex_and_half_close_with_all_socks_bound_address_types() {
    timeout(Duration::from_secs(10), async {
        for address in [
            vec![1, 0, 0, 0, 0],
            vec![4; 17],
            vec![3, 3, b'b', b'n', b'd'],
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let payload: Vec<u8> = (0..=255).cycle().take(256 * 1024).collect();
            let expected = payload.clone();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                request(&mut socket).await;
                let mut reply = vec![5, 0, 0];
                reply.extend(address);
                reply.extend([0, 22]);
                // Fragmentation and binary bytes immediately following the reply are legal.
                for byte in reply {
                    socket.write_all(&[byte]).await.unwrap();
                }
                socket.write_all(b"\x00\xffready").await.unwrap();
                let mut incoming = Vec::new();
                socket.read_to_end(&mut incoming).await.unwrap();
                assert_eq!(incoming, expected);
                socket.write_all(&incoming).await.unwrap();
            });
            let mut child = command(addr).kill_on_drop(true).spawn().unwrap();
            let mut stdin = child.stdin.take().unwrap();
            let mut stdout = child.stdout.take().unwrap();
            let mut prefix = [0; 7];
            // No newline and no stdin yet: verifies that stdout buffering cannot deadlock SSH.
            stdout.read_exact(&mut prefix).await.unwrap();
            assert_eq!(&prefix, b"\x00\xffready");
            let ((), received) = tokio::join!(
                async {
                    stdin.write_all(&payload).await.unwrap();
                    drop(stdin); // half-close must still permit a complete response
                },
                async {
                    let mut output = Vec::new();
                    stdout.read_to_end(&mut output).await.unwrap();
                    output
                }
            );
            assert_eq!(received, payload);
            let result = child.wait_with_output().await.unwrap();
            assert!(result.status.success());
            assert!(
                result.stderr.is_empty(),
                "successful tunnel emitted diagnostics"
            );
            server.await.unwrap();
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn remote_eof_exits_even_when_stdin_remains_open() {
    timeout(Duration::from_secs(5), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            request(&mut socket).await;
            socket
                .write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
            socket.write_all(b"remote closed").await.unwrap();
        });
        let mut child = command(addr).kill_on_drop(true).spawn().unwrap();
        let _open_stdin = child.stdin.take().unwrap();
        let output = child.wait_with_output().await.unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"remote closed");
        server.await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn refusal_invalid_replies_and_timeout_fail_without_stdout() {
    timeout(Duration::from_secs(10), async {
        for reply in [
            vec![5, 2, 0, 1],    // source/target policy denial
            vec![5, 1, 0, 1],    // upstream failure
            vec![4, 0, 0, 1],    // invalid version
            vec![5, 0, 1, 1],    // invalid reserved byte
            vec![5, 0, 0, 9],    // invalid bound address type
            vec![5, 0, 0, 3, 0], // empty bound hostname
            vec![5, 0, 0, 1],    // truncated bound address
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                request(&mut socket).await;
                socket.write_all(&reply).await.unwrap();
            });
            let output = command(addr).kill_on_drop(true).output().await.unwrap();
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            assert!(!output.stderr.is_empty());
            server.await.unwrap();
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut cmd = command(addr);
        cmd.args(["--connect-timeout", "1"]).kill_on_drop(true);
        let child = tokio::spawn(async move { cmd.output().await.unwrap() });
        let (_stalled_socket, _) = listener.accept().await.unwrap();
        let output = child.await.unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("timed out"));
    })
    .await
    .unwrap();
}
