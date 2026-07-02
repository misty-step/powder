use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("powder-server lives two levels below repo root")
        .to_path_buf()
}

#[test]
fn fly_config_keeps_the_instance_always_on_with_data_volume_and_route_checks() {
    let root = repo_root();
    let fly = std::fs::read_to_string(root.join("fly.toml")).expect("read fly.toml");

    for expected in [
        r#"app = "powder""#,
        r#"source = "powder_data""#,
        r#"destination = "/data""#,
        r#"POWDER_DB_PATH = "/data/powder.db""#,
        r#"POWDER_BIND_ADDR = "[::]:4000""#,
        r#"POWDER_PUBLIC_BASE_URL = "http://powder.internal:4000""#,
        r#"POWDER_DISCLOSE_BOOTSTRAP_KEY = "false""#,
        r#"POWDER_REQUIRE_LITESTREAM = "1""#,
        r#"auto_stop_machines = "off""#,
        "auto_start_machines = true",
        "min_machines_running = 1",
        r#"path = "/healthz""#,
        r#"path = "/readyz""#,
    ] {
        assert!(
            fly.contains(expected),
            "fly.toml should contain {expected:?}"
        );
    }
}

#[test]
fn fly_config_is_flycast_only_and_can_never_re_expose_a_public_ip() {
    let root = repo_root();
    let fly = std::fs::read_to_string(root.join("fly.toml")).expect("read fly.toml");

    // `[http_service]` is Fly's public-Anycast-on-80/443 shorthand: defining
    // it asks flyctl to provision a public IP for this app on next deploy if
    // it doesn't have one. This app must only ever declare `[[services]]`,
    // Fly's lower-level stanza that stays private as long as no public IP is
    // allocated (verified live via `bin/check-private-ingress.sh`).
    assert!(
        !fly.contains("[http_service]"),
        "fly.toml must not declare a public [http_service]; use [[services]] for flycast-only ingress"
    );
    assert!(
        !fly.contains("force_https"),
        "fly.toml must not set force_https; Flycast is HTTP-only"
    );
    assert!(
        fly.contains("[[services]]"),
        "fly.toml must declare [[services]] so powder.flycast:4000 stays routable"
    );
    assert!(
        fly.contains(r#"handlers = ["http"]"#) && !fly.contains(r#"handlers = ["tls""#),
        "fly.toml services.ports must use a plain http handler, never a public tls handler"
    );
    // `[::]` is required, not `0.0.0.0`: this app's private-network path
    // (Flycast/`.internal`) is IPv6-only, and a `0.0.0.0` bind cannot answer
    // it (confirmed live: `fly proxy` reset every request against a
    // `0.0.0.0`-bound deploy, and worked immediately once switched to `[::]`).
    // `fly deploy` prints a "not listening on 0.0.0.0" warning for `[::]`
    // regardless; that is a confirmed cosmetic false positive (health
    // checks stay 2/2 passing) — see the comment in fly.toml.
    assert!(
        fly.contains(r#"POWDER_BIND_ADDR = "[::]:4000""#),
        "fly.toml must bind [::] so the private Flycast/.internal path stays reachable"
    );
}

#[test]
fn litestream_config_targets_fly_tigris_with_path_style_s3() {
    let root = repo_root();
    let config = std::fs::read_to_string(root.join("litestream.yml")).expect("read litestream.yml");

    for expected in [
        "path: /data/powder.db",
        "type: s3",
        "bucket: ${BUCKET_NAME}",
        "path: powder.db",
        "endpoint: https://fly.storage.tigris.dev",
        "region: auto",
        "force-path-style: true",
    ] {
        assert!(
            config.contains(expected),
            "litestream.yml should contain {expected:?}"
        );
    }
}

#[test]
fn entrypoint_restore_and_replication_paths_are_locked() {
    let root = repo_root();
    let script = root.join("test/bin/entrypoint_test.sh");
    let output = Command::new("bash")
        .arg(script)
        .current_dir(&root)
        .output()
        .expect("run entrypoint test");

    assert!(
        output.status.success(),
        "entrypoint test failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
