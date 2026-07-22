//! powder-epic-truthful-ops: this file used to assert the shape of "the
//! deploy" with no qualifier, when `fly.toml`/`litestream.yml`/
//! `bin/entrypoint.sh` describe a Fly app (`powder`) that was destroyed
//! 2026-07-07 and is not, and has never been since the 2026-07-09 cutover,
//! this repo's operator's production path -- see
//! `docs/production-deploy.md` for where production actually runs (a
//! DigitalOcean droplet behind Tailscale, supervised by Sanctum, deployed by
//! cross-compile + binary swap, no image build, no Fly step at all).
//!
//! These tests are re-scoped to what `fly.toml`/`litestream.yml`/
//! `bin/entrypoint.sh` actually are: a **supported reference deployment**
//! for a standalone self-hoster who wants to run Powder on Fly under their
//! own org (the quickstart/Docker path in `docs/self-hosting.md` documents
//! the other endorsed option). Keeping this contract green still matters --
//! a self-hoster following the reference should not hit a silently rotted
//! config -- it just no longer stands in for "production is healthy",
//! because it never described the operator's actual production instance.
//!
//! `production_contract.rs`, alongside this file, asserts what the
//! operator's *real* deploy path actually needs from this repo: an
//! env-only-config binary, idempotent migrations, and a schema-gated
//! `/readyz` -- properties that hold regardless of which reference
//! deployment shape (Fly, Docker, bare host) a given instance uses.

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
fn self_hoster_fly_reference_keeps_the_instance_always_on_with_data_volume_and_route_checks() {
    let root = repo_root();
    let fly = std::fs::read_to_string(root.join("fly.toml")).expect("read fly.toml");

    for expected in [
        r#"app = "powder""#,
        r#"source = "powder_data""#,
        r#"destination = "/data""#,
        r#"POWDER_DB_PATH = "/data/powder.db""#,
        r#"POWDER_BIND_ADDR = "[::]:4000""#,
        r#"POWDER_PUBLIC_BASE_URL = "http://powder.internal:4000""#,
        r#"POWDER_REQUIRE_LITESTREAM = "1""#,
        r#"auto_stop_machines = "off""#,
        "auto_start_machines = true",
        "min_machines_running = 1",
        r#"path = "/healthz""#,
        r#"path = "/readyz""#,
    ] {
        assert!(
            fly.contains(expected),
            "fly.toml (the standalone self-hoster reference deployment) should contain {expected:?}"
        );
    }
}

#[test]
fn self_hoster_fly_reference_is_flycast_only_and_can_never_re_expose_a_public_ip() {
    let root = repo_root();
    let fly = std::fs::read_to_string(root.join("fly.toml")).expect("read fly.toml");

    // `[http_service]` is Fly's public-Anycast-on-80/443 shorthand: defining
    // it asks flyctl to provision a public IP for this app on next deploy if
    // it doesn't have one. This reference app must only ever declare
    // `[[services]]`, Fly's lower-level stanza that stays private as long as
    // no public IP is allocated (verified live, prior to the 2026-07-07
    // teardown, via `bin/check-private-ingress.sh`).
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
    // `[::]` is required, not `0.0.0.0`: this reference app's private-network
    // path (Flycast/`.internal`) is IPv6-only, and a `0.0.0.0` bind cannot
    // answer it (confirmed live, prior to the 2026-07-07 teardown: `fly
    // proxy` reset every request against a `0.0.0.0`-bound deploy, and
    // worked immediately once switched to `[::]`). `fly deploy` prints a
    // "not listening on 0.0.0.0" warning for `[::]` regardless; that is a
    // confirmed cosmetic false positive (health checks stayed 2/2 passing
    // while this app was live) — see the comment in fly.toml.
    assert!(
        fly.contains(r#"POWDER_BIND_ADDR = "[::]:4000""#),
        "fly.toml must bind [::] so the private Flycast/.internal path stays reachable"
    );
}

#[test]
fn self_hoster_litestream_reference_targets_fly_tigris_with_path_style_s3() {
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
            "litestream.yml (the standalone self-hoster reference config) should contain {expected:?}"
        );
    }
}

#[test]
fn self_hoster_entrypoint_restore_and_replication_paths_are_locked() {
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
