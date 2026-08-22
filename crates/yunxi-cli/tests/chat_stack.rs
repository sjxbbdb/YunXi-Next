//! Process-level verification of CLI, kernel, plugin protocol, and HTTP client.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn once_mode_crosses_the_isolated_plugin_boundary() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock API");
    listener
        .set_nonblocking(true)
        .expect("make mock API nonblocking");
    let address = listener.local_addr().expect("read mock API address");
    let server = thread::spawn(move || serve_one_request(listener));

    let child = configured_cli(address)
        .args(["--once", "hello from CLI"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch YunXi CLI");
    let output = wait_for_cli(child);

    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let request_body = server.join().expect("join mock API");
    assert_eq!(
        String::from_utf8(output.stdout)
            .expect("UTF-8 CLI output")
            .trim(),
        "fixture reply"
    );
    assert!(request_body.contains("\"model\":\"fixture-model\""));
    assert!(request_body.contains("\"content\":\"hello from CLI\""));
}

#[test]
fn api_failure_is_contained_and_the_plugin_serves_the_next_turn() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock API");
    listener
        .set_nonblocking(true)
        .expect("make mock API nonblocking");
    let address = listener.local_addr().expect("read mock API address");
    let server = thread::spawn(move || {
        let (first_stream, _) = accept_request(&listener);
        write_response(
            first_stream,
            "500 Internal Server Error",
            r#"{"error":{"message":"temporary fixture failure"}}"#,
        );
        let (second_stream, second_body) = accept_request(&listener);
        write_response(
            second_stream,
            "200 OK",
            r#"{"choices":[{"message":{"content":"recovered reply"},"finish_reason":"stop"}]}"#,
        );
        second_body
    });

    let mut child = configured_cli(address)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch YunXi CLI");
    child
        .stdin
        .take()
        .expect("open CLI stdin")
        .write_all(b"first request\nsecond request\n/quit\n")
        .expect("write CLI input");

    let output = wait_for_cli(child);
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let second_body = server.join().expect("join mock API");
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 CLI output");
    assert!(stdout.contains("temporary fixture failure"));
    assert!(stdout.contains("recovered reply"));
    assert!(second_body.contains("\"content\":\"second request\""));
    assert!(!second_body.contains("\"content\":\"first request\""));
}

fn serve_one_request(listener: TcpListener) -> String {
    let (stream, body) = accept_request(&listener);
    write_response(
        stream,
        "200 OK",
        r#"{"choices":[{"message":{"content":"fixture reply"},"finish_reason":"stop"}]}"#,
    );
    body
}

fn accept_request(listener: &TcpListener) -> (std::net::TcpStream, String) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let (stream, _) = loop {
        match listener.accept() {
            Ok(connection) => break connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "mock API received no request");
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("mock API accept failed: {error}"),
        }
    };
    let mut reader = BufReader::new(stream.try_clone().expect("clone API stream"));
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .expect("read API request line");
    assert!(request_line.starts_with("POST /v1/chat/completions "));

    let mut content_length = None;
    let mut authorization = None;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).expect("read API header");
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            match name.trim().to_ascii_lowercase().as_str() {
                "content-length" => {
                    content_length =
                        Some(value.trim().parse::<usize>().expect("valid content length"));
                }
                "authorization" => authorization = Some(value.trim().to_string()),
                _ => {}
            }
        }
    }
    assert_eq!(authorization.as_deref(), Some("Bearer fixture-secret"));

    let mut body = vec![0; content_length.expect("request content length")];
    reader.read_exact(&mut body).expect("read API request body");
    let body = String::from_utf8(body).expect("UTF-8 API request");

    (stream, body)
}

fn write_response(mut stream: std::net::TcpStream, status: &str, response: &str) {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        response.len(),
        response
    )
    .expect("write mock API response");
}

fn configured_cli(address: std::net::SocketAddr) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_yunxi"));
    command
        .env("YUNXI_PROVIDER_PROFILE", "fixture")
        .env("YUNXI_PROVIDER_BASE_URL", format!("http://{address}/v1"))
        .env("YUNXI_PROVIDER_API_KEY", "fixture-secret")
        .env("YUNXI_AGENT_MODEL", "fixture-model")
        .env("YUNXI_PROVIDER_TIMEOUT_MILLIS", "3000")
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost");
    command
}

fn wait_for_cli(mut child: std::process::Child) -> std::process::Output {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if child.try_wait().expect("poll YunXi CLI").is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ignored = child.kill();
            panic!("YunXi CLI did not exit within the test deadline");
        }
        thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output().expect("collect YunXi output")
}
