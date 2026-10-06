//! `verse serve`, driven over a real socket.
//!
//! Everything here goes through the actual binary and the actual network stack:
//! the process is spawned, the discovery file it writes is read, and requests
//! are sent by hand. A `TcpStream` rather than an HTTP client, so that what is
//! being tested is the server and not a library's idea of it.
//!
//! This is the round where an agent *can* verify the end-to-end, unlike the
//! window — so it should.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// Kills the server however the test ends, so a failing assertion does not
/// leave a listener behind for the next run.
struct Server {
    child: Child,
    dir: PathBuf,
    port: u16,
    token: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn start(name: &str) -> Server {
    let dir = std::env::temp_dir().join(format!("verse-serve-test-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");

    let child = Command::new(env!("CARGO_BIN_EXE_verse"))
        .args(["serve", "--port", "0"])
        // Its own data directory, so the test cannot read or overwrite the one
        // belonging to whoever is running it.
        .env("VERSE_CACHE", &dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn verse serve");

    let path = dir.join("serve.json");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }

    let text = std::fs::read_to_string(&path).expect("the discovery file appears");
    let value: serde_json::Value = serde_json::from_str(&text).expect("it is json");

    Server {
        child,
        dir,
        port: value["port"].as_u64().expect("a port") as u16,
        token: value["token"].as_str().expect("a token").to_string(),
    }
}

/// One request on its own connection. Returns the status line and the body.
fn ask(port: u16, request: &str) -> (String, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    stream.write_all(request.as_bytes()).expect("send");

    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read");
    split(&response)
}

fn split(response: &str) -> (String, String) {
    let (head, body) = response.split_once("\r\n\r\n").unwrap_or((response, ""));
    let status = head.lines().next().unwrap_or("").to_string();
    (status, body.to_string())
}

fn get(path: &str, token: Option<&str>) -> String {
    let mut request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n");
    if let Some(token) = token {
        request.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    request.push_str("Connection: close\r\n\r\n");
    request
}

#[test]
fn the_service_starts_reports_itself_and_leaves_a_discovery_file() {
    let server = start("health");

    let (status, _) = ask(server.port, &get("/health", None));
    assert!(status.contains("401"), "no token must be refused: {status}");

    let (status, body) = ask(server.port, &get("/health", Some(&server.token)));
    assert!(status.contains("200"), "with the token: {status} {body}");

    let value: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(value["ok"], serde_json::json!(true));
    assert!(value["service"]["pid"].is_number());
    assert!(
        value["model"].get("present").is_some(),
        "a client needs to know whether the model is here"
    );

    // And the file says the same thing the server does.
    let text = std::fs::read_to_string(server.dir.join("serve.json")).expect("discovery");
    let written: serde_json::Value = serde_json::from_str(&text).expect("json");
    assert_eq!(written["port"], serde_json::json!(server.port));
    assert_eq!(written["instance"], value["service"]["instance"]);
}

#[test]
fn one_connection_serves_several_requests() {
    // A polling client asks every few hundred milliseconds. Reconnecting each
    // time would be a connection per poll for the life of a job.
    let server = start("keepalive");

    let mut stream = TcpStream::connect(("127.0.0.1", server.port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");

    for round in 0..3 {
        let request = format!(
            "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\n\r\n",
            server.token
        );
        stream.write_all(request.as_bytes()).expect("send");

        // Read exactly one response: headers, then the declared body length.
        let mut header = Vec::new();
        let mut byte = [0u8; 1];
        while !header.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).expect("header byte");
            header.push(byte[0]);
        }
        let head = String::from_utf8_lossy(&header).into_owned();
        let length: usize = head
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|value| value.trim().parse().expect("a length"))
            })
            .expect("a Content-Length");

        let mut body = vec![0u8; length];
        stream.read_exact(&mut body).expect("body");

        assert!(
            head.starts_with("HTTP/1.1 200"),
            "round {round} got: {}",
            head.lines().next().unwrap_or("")
        );
        assert!(!String::from_utf8_lossy(&body).is_empty());
    }
}

#[test]
fn a_browser_request_is_refused() {
    // A page cannot read the discovery file, so it cannot have the token. This
    // is the line that does not depend on that being true.
    let server = start("browser");

    let request = format!(
        "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\n\
         Origin: http://evil.test\r\nConnection: close\r\n\r\n",
        server.token
    );
    let (status, body) = ask(server.port, &request);

    assert!(status.contains("403"), "got: {status} {body}");
}

#[test]
fn a_taken_port_fails_loudly_rather_than_sliding() {
    // The dev server already taught this project what a silent port change
    // looks like: a window that attaches itself to somebody else's project.
    let first = start("taken");

    let out = Command::new(env!("CARGO_BIN_EXE_verse"))
        .args(["serve", "--port", &first.port.to_string()])
        .env("VERSE_CACHE", first.dir.join("second"))
        .output()
        .expect("run");

    assert!(!out.status.success(), "the second server must not start");
    let message = String::from_utf8_lossy(&out.stderr);
    assert!(
        message.contains("--port"),
        "and must say what to do about it: {message}"
    );
}

#[test]
fn a_malformed_request_gets_a_400_and_not_a_hang() {
    let server = start("malformed");

    let mut stream = TcpStream::connect(("127.0.0.1", server.port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    stream.write_all(b"not a request\r\n\r\n").expect("send");

    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read");
    assert!(response.starts_with("HTTP/1.1 400"), "got: {response}");
}

#[test]
fn the_discovery_file_is_valid_json_with_everything_a_client_needs() {
    let server = start("discovery");
    let text = std::fs::read_to_string(server.dir.join("serve.json")).expect("read");
    let value: serde_json::Value = serde_json::from_str(&text).expect("json");

    for key in ["version", "host", "port", "token", "pid", "instance", "startedAtMs", "modelsDir"] {
        assert!(value.get(key).is_some(), "missing {key} in {text}");
    }
    assert_eq!(value["host"], serde_json::json!("127.0.0.1"));
    assert_eq!(value["token"].as_str().expect("token").len(), 64);
}
