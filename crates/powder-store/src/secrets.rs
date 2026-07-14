//! Server-side secret scrubbing for high-frequency, agent-authored fields
//! (powder-943). `work_log.body` is raw agent chain-of-thought, appended far
//! more often than a human `comment` and destined to become glass/fleet-retro
//! synthesis input -- credential leakage into that stream is a named,
//! recurring field risk (operator ruling on the card, 2026-07-07), so it gets
//! scrubbed before it ever reaches storage, not after.
//!
//! This is defense in depth against known high-confidence secret shapes, not
//! a guarantee of catching every leak -- the same posture the fleet's other
//! agent-output scrub points (ask-triage, doomscrum) take. Anything shaped
//! like a real credential is redacted; free-form text that merely mentions
//! "the API key" is left alone.

use std::sync::LazyLock;

use regex::Regex;

static PATTERNS: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    let compile = |re: &str| Regex::new(re).expect("secret pattern is valid regex");
    vec![
        ("openai-key", compile(r"sk-[A-Za-z0-9]{20,}")),
        ("anthropic-key", compile(r"sk-ant-[A-Za-z0-9_\-]{20,}")),
        ("github-token", compile(r"gh[pousr]_[A-Za-z0-9]{20,}")),
        ("aws-access-key-id", compile(r"AKIA[0-9A-Z]{16}")),
        ("slack-token", compile(r"xox[baprs]-[A-Za-z0-9\-]{10,}")),
        (
            "bearer-token",
            compile(r"(?i)bearer\s+[A-Za-z0-9\-_.]{20,}"),
        ),
        // Powder's own API key and webhook signing-secret shapes
        // (`sk_powder_...` / `whsec_powder_...`, minted by
        // `identity::Store::create_api_key` / `events::create_event_subscription`
        // with a 32-char nanoid body -- well over the 20-char floor here).
        // No `Bearer`/prefix requirement: these patterns must fire mid-line
        // in a bare env-dump paste like `POWDER_API_KEY=sk_powder_...`, not
        // only after a header-style label.
        ("powder-api-key", compile(r"sk_powder_[A-Za-z0-9_\-]{20,}")),
        (
            "powder-webhook-secret",
            compile(r"whsec_powder_[A-Za-z0-9_\-]{20,}"),
        ),
        (
            "private-key-block",
            compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----"),
        ),
    ]
});

/// Replace every known secret shape in `body` with `[REDACTED:<pattern>]`,
/// leaving everything else untouched.
pub fn scrub_secrets(body: &str) -> String {
    let mut scrubbed = body.to_string();
    for (name, regex) in PATTERNS.iter() {
        if regex.is_match(&scrubbed) {
            let replacement = format!("[REDACTED:{name}]");
            scrubbed = regex
                .replace_all(&scrubbed, replacement.as_str())
                .into_owned();
        }
    }
    scrubbed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_an_openai_style_key() {
        let scrubbed = scrub_secrets("using sk-abcdefghijklmnopqrstuvwxyz123456 to auth");
        assert!(!scrubbed.contains("sk-abcdefghijklmnopqrstuvwxyz123456"));
        assert!(scrubbed.contains("[REDACTED:openai-key]"));
    }

    #[test]
    fn redacts_an_anthropic_style_key() {
        let scrubbed = scrub_secrets("ANTHROPIC_API_KEY=sk-ant-api03-abcdefghijklmnopqrstuvwxyz"); // leak-gate:allow synthetic fixture, not a live credential
        assert!(!scrubbed.contains("sk-ant-api03-abcdefghijklmnopqrstuvwxyz"));
        assert!(scrubbed.contains("[REDACTED:anthropic-key]"));
    }

    #[test]
    fn redacts_a_github_token() {
        let scrubbed = scrub_secrets("token: ghp_abcdefghijklmnopqrstuvwxyz0123456789");
        assert!(scrubbed.contains("[REDACTED:github-token]"));
        assert!(!scrubbed.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));
    }

    #[test]
    fn redacts_a_private_key_block() {
        let key =
            "-----BEGIN RSA PRIVATE KEY-----\nMIIBOgIBAAJBAK...\n-----END RSA PRIVATE KEY-----";
        let scrubbed = scrub_secrets(key);
        assert!(!scrubbed.contains("MIIBOgIBAAJBAK"));
        assert!(scrubbed.contains("[REDACTED:private-key-block]"));
    }

    #[test]
    fn leaves_ordinary_prose_about_keys_untouched() {
        let body = "spent the last hour debugging why the API key wasn't loading from the env";
        assert_eq!(scrub_secrets(body), body);
    }

    /// Build a fake secret from split literals so the leak-gate's own
    /// pattern scan of this source file never matches a contiguous
    /// secret-shaped string (same convention `scripts/leak-gate.sh` uses
    /// for its self-test fixtures).
    fn fake_powder_key() -> String {
        format!("sk_powder_{}", "a".repeat(32))
    }

    fn fake_powder_whsec() -> String {
        format!("whsec_powder_{}", "b".repeat(32))
    }

    #[test]
    fn redacts_a_powder_api_key() {
        let key = fake_powder_key();
        let scrubbed = scrub_secrets(&format!("here is the key: {key}"));
        assert!(!scrubbed.contains(&key));
        assert!(scrubbed.contains("[REDACTED:powder-api-key]"));
    }

    #[test]
    fn redacts_a_powder_webhook_secret() {
        let secret = fake_powder_whsec();
        let scrubbed = scrub_secrets(&format!("signing secret: {secret}"));
        assert!(!scrubbed.contains(&secret));
        assert!(scrubbed.contains("[REDACTED:powder-webhook-secret]"));
    }

    #[test]
    fn redacts_a_bare_env_dump_line_without_bearer_or_label() {
        let key = fake_powder_key();
        let scrubbed = scrub_secrets(&format!("POWDER_API_KEY={key}"));
        assert!(!scrubbed.contains(&key));
        assert!(scrubbed.contains("[REDACTED:powder-api-key]"));
    }

    #[test]
    fn leaves_a_short_prose_mention_of_the_powder_key_prefix_untouched() {
        // Fewer than 20 chars after the prefix: a work-log note that merely
        // discusses the key *shape* in prose, not a real credential, must
        // survive scrubbing untouched.
        let body = "the sk_powder_ prefix identifies Powder-issued API keys";
        assert_eq!(scrub_secrets(body), body);
    }
}
