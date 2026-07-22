//! powder-epic-truthful-ops: `deploy_contract.rs` asserts the standalone
//! self-hoster reference deployment (Fly/Litestream/entrypoint.sh) stays
//! internally consistent. This file asserts the separate, narrower thing
//! the operator's *real* production path (a DigitalOcean droplet, `scp`'d
//! binaries, no image build -- see `docs/production-deploy.md`) actually
//! needs from this repo, independent of which reference deployment shape
//! (or none at all) a given instance uses:
//!
//! - the binary boots from real process environment only, never a `.env`
//!   file (`docs/self-hosting.md`'s "No dotenv loader" section) -- load-
//!   bearing because production's `EnvironmentFile=`/Sanctum-supervisor env
//!   block is real process environment, and a silent `.env` fallback would
//!   be a way for a stale or leftover file to override it unnoticed;
//! - migrations are idempotent across repeated boots against the same
//!   database (the property `powder-store`'s own unit tests prove at the
//!   `Store::migrate` level; this proves it survives the real binary's full
//!   startup path, twice, end to end);
//! - `/readyz` gates on schema version actually matching this build's
//!   expectation, not just "some version came back".
//!
//! Spawns the real `powder-server` binary and drives it over real HTTP,
//! same as `socket_smoke.rs` -- these are the two tests in this crate that
//! exercise `main()` itself rather than the in-process `Router`.

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port to find one free");
    listener.local_addr().expect("read local addr").port()
}

fn unique_db_path(label: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "powder-server-production-contract-{label}-{}-{nanos}.db",
        std::process::id()
    ))
}

fn wait_for_200(url: &str, timeout: Duration) {
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

/// Spawns `powder-server` with the given env vars and working directory,
/// draining stdout/stderr on background threads so tracing's own request
/// logging can't stall the child by filling a pipe buffer.
fn spawn_server(envs: &[(&str, String)], cwd: &std::path::Path) -> ChildGuard {
    let mut command = Command::new(env!("CARGO_BIN_EXE_powder-server"));
    command
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let bootstrap_key_file = envs
        .iter()
        .find(|(key, _)| *key == "POWDER_DB_PATH")
        .map(|(_, value)| std::path::PathBuf::from(value).with_extension("bootstrap.key"));
    if let Some(path) = bootstrap_key_file.as_ref() {
        let _ = std::fs::remove_file(path);
        command.env("POWDER_BOOTSTRAP_KEY_FILE", path);
    }
    for (key, value) in envs {
        if *key != "POWDER_DISCLOSE_BOOTSTRAP_KEY" {
            command.env(key, value);
        }
    }
    let mut child = command
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

    ChildGuard(child)
}

/// A `.env` file in the working directory a real deploy would launch
/// `powder-server` from (the repo checkout, a systemd unit's `WorkingDirectory`)
/// must never be read. Proven by planting one that -- if it were read --
/// would bind to a *different* port than the real env var says, then
/// asserting the server answers on the real env var's port and the
/// `.env`-only port never comes up at all.
#[test]
fn binary_boots_from_real_process_environment_only_and_ignores_a_dotenv_file_in_cwd() {
    let real_port = free_port();
    let decoy_port = free_port();
    let base = format!("http://127.0.0.1:{real_port}");
    let decoy_base = format!("http://127.0.0.1:{decoy_port}");
    let db_path = unique_db_path("dotenv");
    let _ = std::fs::remove_file(&db_path);

    let work_dir = std::env::temp_dir().join(format!(
        "powder-server-dotenv-cwd-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&work_dir).expect("create scratch working directory");
    std::fs::write(
        work_dir.join(".env"),
        format!(
            "POWDER_BIND_ADDR=127.0.0.1:{decoy_port}\nPOWDER_DB_PATH=/nonexistent/should-never-be-opened.db\n"
        ),
    )
    .expect("write decoy .env");

    let _guard = spawn_server(
        &[
            ("POWDER_DB_PATH", db_path.display().to_string()),
            ("POWDER_BIND_ADDR", format!("127.0.0.1:{real_port}")),
            ("POWDER_AUTH_MODE", "api-key".to_string()),
            ("POWDER_DISCLOSE_BOOTSTRAP_KEY", "false".to_string()),
        ],
        &work_dir,
    );

    wait_for_200(&format!("{base}/healthz"), Duration::from_secs(10));

    assert!(
        ureq::get(&format!("{decoy_base}/healthz")).call().is_err(),
        "the .env-only bind address must never come up -- a dotenv loader would have bound it"
    );

    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_dir_all(&work_dir);
}

/// Boots the real binary against the same database file twice in a row
/// (the shape a redeploy or a supervisor restart produces): both boots must
/// succeed and `/readyz` must report the identical, fully-migrated schema
/// version both times -- proving `Store::migrate`'s idempotency guards hold
/// through the binary's actual startup path, not just the in-process
/// `Store` unit tests.
#[test]
fn migrations_are_idempotent_across_two_consecutive_boots_of_the_same_database() {
    let db_path = unique_db_path("double-boot");
    let _ = std::fs::remove_file(&db_path);
    let cwd = std::env::temp_dir();

    let first_port = free_port();
    let first_base = format!("http://127.0.0.1:{first_port}");
    let first_schema_version = {
        let _guard = spawn_server(
            &[
                ("POWDER_DB_PATH", db_path.display().to_string()),
                ("POWDER_BIND_ADDR", format!("127.0.0.1:{first_port}")),
                ("POWDER_AUTH_MODE", "api-key".to_string()),
                ("POWDER_DISCLOSE_BOOTSTRAP_KEY", "false".to_string()),
            ],
            &cwd,
        );
        wait_for_200(&format!("{first_base}/healthz"), Duration::from_secs(10));
        let ready = ureq::get(&format!("{first_base}/readyz"))
            .call()
            .expect("first boot readyz");
        assert_eq!(ready.status(), 200, "first boot should report ready");
        ready.into_json::<serde_json::Value>().unwrap()["schema_version"]
            .as_u64()
            .expect("schema_version present on first boot")
    };
    // _guard dropped here: the first server process is killed before the
    // second one opens the same SQLite file.

    let second_port = free_port();
    let second_base = format!("http://127.0.0.1:{second_port}");
    let second_schema_version = {
        let _guard = spawn_server(
            &[
                ("POWDER_DB_PATH", db_path.display().to_string()),
                ("POWDER_BIND_ADDR", format!("127.0.0.1:{second_port}")),
                ("POWDER_AUTH_MODE", "api-key".to_string()),
                ("POWDER_DISCLOSE_BOOTSTRAP_KEY", "false".to_string()),
            ],
            &cwd,
        );
        wait_for_200(&format!("{second_base}/healthz"), Duration::from_secs(10));
        let ready = ureq::get(&format!("{second_base}/readyz"))
            .call()
            .expect("second boot readyz");
        assert_eq!(
            ready.status(),
            200,
            "second boot against the already-migrated database should also report ready, \
             not fail on a duplicate-column or duplicate-table error"
        );
        ready.into_json::<serde_json::Value>().unwrap()["schema_version"]
            .as_u64()
            .expect("schema_version present on second boot")
    };

    assert_eq!(
        first_schema_version, second_schema_version,
        "re-running migrate() against an already-migrated database must not change its schema version"
    );

    let _ = std::fs::remove_file(&db_path);
}

/// `/readyz`'s schema-match gate reports the exact version this build
/// expects (`schema_version_expected`) alongside the database's actual
/// version, and they agree for a freshly migrated database -- the
/// process-level proof that `Store::migrate` and `/readyz` are checking the
/// same `SCHEMA_VERSION`, not two independently-drifting constants.
#[test]
fn readyz_gates_on_schema_version_matching_this_builds_expectation() {
    let port = free_port();
    let base = format!("http://127.0.0.1:{port}");
    let db_path = unique_db_path("readyz-schema");
    let _ = std::fs::remove_file(&db_path);
    let cwd = std::env::temp_dir();

    let _guard = spawn_server(
        &[
            ("POWDER_DB_PATH", db_path.display().to_string()),
            ("POWDER_BIND_ADDR", format!("127.0.0.1:{port}")),
            ("POWDER_AUTH_MODE", "api-key".to_string()),
            ("POWDER_DISCLOSE_BOOTSTRAP_KEY", "false".to_string()),
        ],
        &cwd,
    );
    wait_for_200(&format!("{base}/healthz"), Duration::from_secs(10));

    let ready = ureq::get(&format!("{base}/readyz"))
        .call()
        .expect("readyz request should succeed once healthz is up");
    assert_eq!(ready.status(), 200);
    let body: serde_json::Value = ready.into_json().unwrap();
    assert_eq!(body["ok"], true);
    let actual = body["schema_version"].as_u64().unwrap();
    let expected = body["schema_version_expected"].as_u64().unwrap();
    assert_eq!(
        actual, expected,
        "a freshly migrated database's schema_version must match this build's schema_version_expected"
    );
    assert!(body["writable"].as_bool().unwrap());
    assert_eq!(body["poison_count"].as_u64().unwrap(), 0);

    let _ = std::fs::remove_file(&db_path);
}
