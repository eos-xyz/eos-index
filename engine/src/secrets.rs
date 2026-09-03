//!  — secret detection: known credential formats left in HEAD text
//! files. A defensive-security signal ("do we have leaked keys?"), git-native and
//! precise: only well-known token SHAPES (prefix + charset + length) match, so
//! false positives stay near zero. We store the rule, the line, and a MASKED
//! preview (a type prefix + `…`) — never the secret itself.
//!
//! Content scan (mid+ tier); the bench verifies each finding is really a token of
//! that shape at that line of the HEAD blob.

use std::path::Path;

use anyhow::Result;

/// A finding before file_id assignment (path-keyed; stitched in `main`).
pub struct SecretRaw {
    pub path: String,
    pub line: i32,
    pub rule: String,
    pub preview: String,
}

fn is_binary(b: &[u8]) -> bool {
    b[..b.len().min(8000)].contains(&0)
}

/// Length of the run of `pred`-matching bytes starting at `i`.
fn run_len(b: &[u8], i: usize, pred: impl Fn(u8) -> bool) -> usize {
    let mut n = 0;
    while i + n < b.len() && pred(b[i + n]) {
        n += 1;
    }
    n
}

fn alnum(c: u8) -> bool {
    c.is_ascii_alphanumeric()
}
fn upper_alnum(c: u8) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit()
}
fn token_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-'
}

/// The detectors: (rule, prefix, char predicate, min body length, exact?). A hit
/// is the prefix followed by at least (or exactly) that many body chars, and the
/// NEXT char must not extend the token (so a longer look-alike doesn't match).
struct Det {
    rule: &'static str,
    prefix: &'static str,
    pred: fn(u8) -> bool,
    min: usize,
    exact: bool,
}

const DETECTORS: &[Det] = &[
    Det { rule: "aws_access_key", prefix: "AKIA", pred: upper_alnum, min: 16, exact: true },
    Det { rule: "github_token", prefix: "ghp_", pred: alnum, min: 36, exact: false },
    Det { rule: "github_token", prefix: "gho_", pred: alnum, min: 36, exact: false },
    Det { rule: "github_token", prefix: "ghs_", pred: alnum, min: 36, exact: false },
    Det { rule: "github_token", prefix: "ghr_", pred: alnum, min: 36, exact: false },
    Det { rule: "github_token", prefix: "ghu_", pred: alnum, min: 36, exact: false },
    Det { rule: "google_api_key", prefix: "AIza", pred: token_char, min: 35, exact: true },
    Det { rule: "slack_token", prefix: "xoxb-", pred: token_char, min: 10, exact: false },
    Det { rule: "slack_token", prefix: "xoxp-", pred: token_char, min: 10, exact: false },
    Det { rule: "stripe_secret_key", prefix: "sk_live_", pred: alnum, min: 20, exact: false },
];

/// Findings in one line. Prefix detectors + a private-key header substring.
fn scan_line(line: &str) -> Vec<(&'static str, String)> {
    let b = line.as_bytes();
    let mut hits = Vec::new();
    for d in DETECTORS {
        let pfx = d.prefix.as_bytes();
        let mut i = 0;
        while i + pfx.len() <= b.len() {
            if &b[i..i + pfx.len()] == pfx {
                let body = i + pfx.len();
                let n = run_len(b, body, d.pred);
                let ok = if d.exact { n == d.min } else { n >= d.min };
                // The char right after the body must not itself be a body char
                // (else a longer run was truncated at `min` and this is a false
                // exact match / partial). For exact, run_len already stops at the
                // first non-pred char, so n==min means the token is exactly min.
                if ok {
                    hits.push((d.rule, format!("{}…", &line[i..body])));
                    i = body + n;
                    continue;
                }
            }
            i += 1;
        }
    }
    if line.contains("-----BEGIN ") && line.contains("PRIVATE KEY-----") {
        hits.push(("private_key", "-----BEGIN …PRIVATE KEY-----".to_string()));
    }
    hits
}

/// Scan every HEAD text blob for known secret shapes. Parallelized over HEAD blobs
/// (order-preserving, so identical output).
pub fn compute(repo_path: &Path) -> Result<Vec<SecretRaw>> {
    crate::blame::par_head_blobs(repo_path, |path, data| {
        let mut out = Vec::new();
        if is_binary(data) {
            return out;
        }
        let text = String::from_utf8_lossy(data);
        for (idx, line) in text.lines().enumerate() {
            for (rule, preview) in scan_line(line) {
                out.push(SecretRaw { path: path.to_string(), line: idx as i32 + 1, rule: rule.to_string(), preview });
            }
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::scan_line;

    #[test]
    fn detects_known_shapes_only() {
        assert_eq!(scan_line("key = AKIAIOSFODNN7EXAMPLE").len(), 1); // 16 body
        assert_eq!(scan_line("AKIASHORT").len(), 0); // too short
        assert_eq!(scan_line("token: ghp_0123456789abcdefghij0123456789ABCDEF").len(), 1); // 36
        assert_eq!(scan_line("g = AIzaSyA1234567890abcdefghijklmnopqrstuv").len(), 1); // 35 body
        assert_eq!(scan_line("no secrets here, just text").len(), 0);
        assert_eq!(scan_line("-----BEGIN RSA PRIVATE KEY-----").len(), 1);
        // masked preview never leaks the body
        let h = scan_line("AKIAIOSFODNN7EXAMPLE");
        assert_eq!(h[0].1, "AKIA…");
    }
}
