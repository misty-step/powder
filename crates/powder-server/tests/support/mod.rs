//! Shared scaffolding for socket-level integration tests -- the ones that
//! boot the real `powder-server` binary on a real TCP port instead of
//! driving the axum `Router` in-process. Lifted out of `socket_smoke.rs`
//! (per its own instruction) when `sse_live.rs` became the second such test.
use std::io::BufRead;
use std::io::BufReader;
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
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

pub struct RunningServer {
    /// Owns the child process; dropping the struct kills the server.
    pub _guard: ChildGuard,
    pub base: String,
    pub bootstrap_key: String,
    pub db_path: std::path::PathBuf,
    pub bootstrap_key_file: std::path::PathBuf,
}

/// Boots the real `powder-server` binary on a free port with a throwaway
/// database, drains its stdio (so tracing can't fill a pipe buffer and stall
/// it), reads the one-time bootstrap key from a 0600 file, and waits for
/// `/healthz` before returning.
pub fn spawn_server(label: &str) -> RunningServer {
    let port = free_port();
    let base = format!("http://127.0.0.1:{port}");
    let db_path = unique_db_path(label);
    let bootstrap_key_file = db_path.with_extension("bootstrap.key");
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&bootstrap_key_file);

    let mut child = Command::new(env!("CARGO_BIN_EXE_powder-server"))
        .env("POWDER_DB_PATH", &db_path)
        .env("POWDER_BOOTSTRAP_KEY_FILE", &bootstrap_key_file)
        .env("POWDER_BIND_ADDR", format!("127.0.0.1:{port}"))
        .env("POWDER_AUTH_MODE", "api-key")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the powder-server binary under test");

    let stdout = child.stdout.take().expect("child stdout was piped");
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if line.is_err() {
                break;
            }
        }
    });
    let stderr = child.stderr.take().expect("child stderr was piped");
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            if line.is_err() {
                break;
            }
        }
    });

    let guard = ChildGuard(child);
    wait_for_200(&format!("{base}/healthz"), Duration::from_secs(10));
    let bootstrap_key = std::fs::read_to_string(&bootstrap_key_file)
        .expect("read the one-shot bootstrap API key file")
        .trim()
        .to_string();
    assert!(!bootstrap_key.is_empty());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&bootstrap_key_file)
            .expect("stat bootstrap key file")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "bootstrap key file must be owner-only");
    }

    RunningServer {
        _guard: guard,
        base,
        bootstrap_key,
        db_path,
        bootstrap_key_file,
    }
}
