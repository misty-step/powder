use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub struct ChildGuard(pub Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

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
    pub _guard: ChildGuard,
    pub base: String,
    pub bootstrap_key: String,
    pub db_path: std::path::PathBuf,
    pub bootstrap_key_path: std::path::PathBuf,
    pub logs: Arc<Mutex<String>>,
}

pub fn spawn_server(label: &str) -> RunningServer {
    let port = free_port();
    let base = format!("http://127.0.0.1:{port}");
    let db_path = unique_db_path(label);
    let bootstrap_key_path = db_path.with_extension("bootstrap-key");
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&bootstrap_key_path);

    let mut child = Command::new(env!("CARGO_BIN_EXE_powder-server"))
        .env("POWDER_DB_PATH", &db_path)
        .env("POWDER_BIND_ADDR", format!("127.0.0.1:{port}"))
        .env("POWDER_AUTH_MODE", "api-key")
        .env("POWDER_BOOTSTRAP_KEY_FILE", &bootstrap_key_path)
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

    let logs = Arc::new(Mutex::new(String::new()));
    let captured_logs = Arc::clone(&logs);
    let stderr = child.stderr.take().expect("child stderr was piped");
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            captured_logs
                .lock()
                .expect("capture log mutex")
                .push_str(&format!("{line}\n"));
        }
    });

    let guard = ChildGuard(child);
    let deadline = Instant::now() + Duration::from_secs(10);
    let bootstrap_key = loop {
        if let Ok(raw) = std::fs::read_to_string(&bootstrap_key_path) {
            let raw = raw.trim().to_string();
            if !raw.is_empty() {
                break raw;
            }
        }
        if Instant::now() >= deadline {
            panic!("read the 0600 bootstrap key file before timeout");
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    wait_for_200(&format!("{base}/healthz"), Duration::from_secs(10));

    RunningServer {
        _guard: guard,
        base,
        bootstrap_key,
        db_path,
        bootstrap_key_path,
        logs,
    }
}
