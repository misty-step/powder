use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    process::Command,
    thread,
};

#[test]
fn remote_http_errors_surface_body_and_exit_nonzero() {
    let output = run_remote_update(403, r#"{"error":"policy denies this status change"}"#);

    assert!(
        !output.status.success(),
        "HTTP errors must fail the process"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("policy denies this status change"),
        "stderr should include the server reason: {stderr}"
    );
}

#[test]
fn final_three_xx_http_errors_surface_body_and_exit_nonzero() {
    let output = run_remote_update(300, r#"{"error":"policy denies this redirect"}"#);

    assert!(
        !output.status.success(),
        "final 3xx responses must fail the process"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("policy denies this redirect"),
        "stderr should include the server reason: {stderr}"
    );
}

fn run_remote_update(status: u16, response_body: &str) -> std::process::Output {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
    let address = listener.local_addr().expect("test server address");
    let response_body = response_body.to_owned();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept CLI request");
        let mut reader = BufReader::new(stream.try_clone().expect("clone request stream"));
        let mut request_line = String::new();
        reader
            .read_line(&mut request_line)
            .expect("read request line");
        let mut content_length = 0usize;
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).expect("read request header");
            if header == "\r\n" {
                break;
            }
            if let Some(value) = header.strip_prefix("Content-Length:") {
                content_length = value.trim().parse().expect("content length");
            }
        }
        let mut body = vec![0; content_length];
        reader.read_exact(&mut body).expect("read request body");

        let response = format!(
            "HTTP/1.1 {status} Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write error response");
    });

    let output = Command::new(env!("CARGO_BIN_EXE_powder"))
        .args(["update-card", "card-1", "--status", "ready"])
        .env("POWDER_API_BASE_URL", format!("http://{address}"))
        .env("POWDER_API_KEY", "sk_powder_test")
        .output()
        .expect("run powder CLI");
    server.join().expect("test server must finish");
    output
}
