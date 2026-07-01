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
        r#"POWDER_PUBLIC_BASE_URL = "https://powder.internal""#,
        r#"POWDER_DISCLOSE_BOOTSTRAP_KEY = "false""#,
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
