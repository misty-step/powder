//! Shared scaffolding for socket-level integration tests -- the ones that
//! boot the real `powder-server` binary on a real TCP port instead of
//! driving the axum `Router` in-process. Lifted out of `socket_smoke.rs`
//! (per its own instruction) when `sse_live.rs` became the second such test.
use std::io::BufRead;
use std::io::BufReader;
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Kills the child `powder-server` process (and reaps it) on drop, including
/// when an `assert_eq!`/`expect` panics mid-test -- otherwise a failing
/// assertion would leak a live server process and a bound port past the end
/// of the test.
pub struct ChildGuard(pub Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `powder-server`'s `Config::from_env` requires `POWDER_BIND_ADDR` to parse
/// as a full `SocketAddr` (host and port), and `main()` does not log the
/// address it actually bound -- so `POWDER_BIND_ADDR=127.0.0.1:0` plus a
/// log-scrape for the OS-assigned port is not available here. Instead, bind
/// a throwaway listener to port 0, read back the OS-assigned port, and drop
/// the listener immediately so the server can bind it. This has a TOCTOU
/// race (another process could grab the same port between the drop and the
/// server's own `bind()`); accepted for a single-process local/CI test suite
/// where that window is a handful of microseconds, over adding a
/// log-scraping dependency for a smoke test.
pub fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port to find one free");
    listener.local_addr().expect("read local addr").port()
}

pub fn unique_db_path(label: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "powder-server-{label}-{}-{nanos}.db",
        std::process::id()
    ))
}

pub fn wait_for_200(url: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(response) = ureq::get(url).call() {
            if response.status() == 200 {
                return;
            }
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for a 200 from {url} within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub const BOOTSTRAP_PREFIX: &str = "Powder bootstrap API key: ";

pub struct RunningServer {
    /// Owns the child process; dropping the struct kills the server.
    pub _guard: ChildGuard,
    pub base: String,
    pub bootstrap_key: String,
    pub db_path: std::path::PathBuf,
}

/// Boots the real `powder-server` binary on a free port with a throwaway
/// database, drains its stdio (so tracing can't fill a pipe buffer and stall
/// it), captures the one-time bootstrap key from stderr, and waits for
/// `/healthz` before returning.
pub fn spawn_server(label: &str) -> RunningServer {
    let port = free_port();
    let base = format!("http://127.0.0.1:{port}");
    let db_path = unique_db_path(label);
    let _ = std::fs::remove_file(&db_path);

    let mut child = Command::new(env!("CARGO_BIN_EXE_powder-server"))
        .env("POWDER_DB_PATH", &db_path)
        .env("POWDER_BIND_ADDR", format!("127.0.0.1:{port}"))
        .env("POWDER_AUTH_MODE", "api-key")
        .env("POWDER_DISCLOSE_BOOTSTRAP_KEY", "true")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the powder-server binary under test");

    // Drain stdout on its own thread so tracing's normal request logging
    // can't fill the pipe buffer and stall the server.
    let stdout = child.stdout.take().expect("child stdout was piped");
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if line.is_err() {
                break;
            }
        }
    });

    // stderr carries the one-time bootstrap-key line (POWDER_DISCLOSE_
    // BOOTSTRAP_KEY=true); scan for it on a background thread and keep
    // draining afterward for the same buffer-stall reason as stdout.
    let stderr = child.stderr.take().expect("child stderr was piped");
    let (key_tx, key_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut sent = false;
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            if !sent {
                if let Some(key) = line.strip_prefix(BOOTSTRAP_PREFIX) {
                    sent = key_tx.send(key.trim().to_string()).is_ok();
                }
            }
        }
    });

    // From here on, every early return (including a panicking assert) must
    // still kill the child -- hand it to the guard now.
    let guard = ChildGuard(child);

    let bootstrap_key = key_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("read the printed bootstrap API key from server stderr before timeout");

    wait_for_200(&format!("{base}/healthz"), Duration::from_secs(10));

    RunningServer {
        _guard: guard,
        base,
        bootstrap_key,
        db_path,
    }
}
