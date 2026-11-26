//! Secret redaction: the single path every log line, UI event, WebSocket frame and crash
//! report goes through (AGENTS.md invariant 4).
//!
//! The rules are mirrored by `harness/src/lapis_harness/redact.py`; both are tested against
//! `tests/vectors/redaction_vectors.json`, so the regexes below stay within the syntax that
//! Rust's `regex` and Python's `re` read the same way.

use std::borrow::Cow;
use std::sync::{OnceLock, RwLock};

use regex::{Captures, Regex};
use serde_json::Value;

pub const MARK: &str = "[REDACTED]";
pub const PEM_MARK: &str = "[REDACTED PRIVATE KEY]";
/// Registered values shorter than this are ignored: they would erase ordinary words.
pub const MIN_KNOWN_LEN: usize = 8;

const PEM_BLOCK: &str =
    r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z0-9 ]*PRIVATE KEY-----";
const PEM_CUT: &str = r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*";
const AUTH_HEADER: &str = r"(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/=-]{8,}";
const TOKEN_FORMATS: &str = concat!(
    r"\b(?:sk-ant-[A-Za-z0-9_-]{16,}|sk-[A-Za-z0-9_-]{20,}|AIza[0-9A-Za-z_-]{30,}",
    r"|tskey-[A-Za-z0-9_-]{8,}|gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}",
    r"|xox[abprs]-[A-Za-z0-9-]{10,}|AKIA[0-9A-Z]{16})"
);
const KEY_VALUE: &str = concat!(
    r#"(?i)\b([A-Za-z0-9_.-]*?(?:api[_-]?key|secret|token|password|passwd|psk|authkey|private[_-]?key))"#,
    r#"(["']?)(\s*[:=]\s*)("(?:[^"\\]|\\.)*"|'[^']*'|[^\s"',;}&]+)"#
);
const SECRET_KEY: &str = r"(?i)^[A-Za-z0-9_.-]*(?:api[_-]?key|secret|token|password|passwd|psk|authkey|private[_-]?key)$";

struct Rules {
    pem_block: Regex,
    pem_cut: Regex,
    auth: Regex,
    tokens: Regex,
    key_value: Regex,
    secret_key: Regex,
}

fn rules() -> Option<&'static Rules> {
    static RULES: OnceLock<Option<Rules>> = OnceLock::new();
    RULES
        .get_or_init(|| {
            Some(Rules {
                pem_block: Regex::new(PEM_BLOCK).ok()?,
                pem_cut: Regex::new(PEM_CUT).ok()?,
                auth: Regex::new(AUTH_HEADER).ok()?,
                tokens: Regex::new(TOKEN_FORMATS).ok()?,
                key_value: Regex::new(KEY_VALUE).ok()?,
                secret_key: Regex::new(SECRET_KEY).ok()?,
            })
        })
        .as_ref()
}

fn known() -> &'static RwLock<Vec<String>> {
    static KNOWN: OnceLock<RwLock<Vec<String>>> = OnceLock::new();
    KNOWN.get_or_init(|| RwLock::new(Vec::new()))
}

/// Remember a secret value (a loaded API key, a minted bearer token) so it is scrubbed
/// verbatim wherever it shows up, whatever its format.
pub fn register_secret(value: &str) {
    let value = value.trim();
    if value.len() < MIN_KNOWN_LEN {
        return;
    }
    if let Ok(mut known) = known().write() {
        if !known.iter().any(|k| k == value) {
            known.push(value.to_owned());
            // Longest first, so a secret that contains another is removed whole.
            known.sort_by_key(|k| std::cmp::Reverse(k.len()));
        }
    }
}

fn key_value_replacement(caps: &Captures<'_>) -> String {
    let value = &caps[4];
    let kept = matches!(
        value.to_ascii_lowercase().as_str(),
        "null" | "true" | "false"
    ) || value == "\"\""
        || value == "''";
    let shown = if kept {
        value.to_owned()
    } else if value.starts_with('"') {
        format!("\"{MARK}\"")
    } else if value.starts_with('\'') {
        format!("'{MARK}'")
    } else {
        MARK.to_owned()
    };
    format!("{}{}{}{}", &caps[1], &caps[2], &caps[3], shown)
}

/// Text with every recognised secret replaced by `[REDACTED]`.
pub fn redact(text: &str) -> String {
    let mut out: Cow<'_, str> = Cow::Borrowed(text);
    if let Ok(known) = known().read() {
        for secret in known.iter() {
            if out.contains(secret.as_str()) {
                out = Cow::Owned(out.replace(secret.as_str(), MARK));
            }
        }
    }
    let Some(r) = rules() else {
        // The patterns are constants and compile; if they ever did not, show nothing.
        return MARK.to_owned();
    };
    let out = r.pem_block.replace_all(&out, PEM_MARK).into_owned();
    let out = r.pem_cut.replace_all(&out, PEM_MARK).into_owned();
    let out = r
        .auth
        .replace_all(&out, |c: &Captures<'_>| format!("{} {MARK}", &c[1]))
        .into_owned();
    let out = r.tokens.replace_all(&out, MARK).into_owned();
    r.key_value
        .replace_all(&out, key_value_replacement)
        .into_owned()
}

/// A JSON value with secret-named fields blanked and every string redacted, structure kept.
pub fn redact_json(value: &mut Value) {
    match value {
        Value::String(s) => {
            let r = redact(s);
            if r != *s {
                *s = r;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact_json),
        Value::Object(map) => {
            let secret_key = rules().map(|r| &r.secret_key);
            for (key, v) in map.iter_mut() {
                let is_secret = secret_key.is_none_or(|re| re.is_match(key));
                let blank = match v {
                    Value::Null | Value::Bool(_) => false,
                    Value::String(s) => !s.is_empty(),
                    _ => true,
                };
                if is_secret && blank {
                    *v = Value::String(MARK.to_owned());
                } else {
                    redact_json(v);
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_secrets_are_scrubbed_verbatim() {
        register_secret("short");
        register_secret("plainvalue-without-format-1234");
        assert_eq!(redact("short"), "short");
        assert_eq!(
            redact("x plainvalue-without-format-1234 y"),
            "x [REDACTED] y"
        );
    }
}
