//!  — technical-debt markers: the `TODO`/`FIXME`/`HACK`/`XXX`/`NOTE`
//! left in HEAD text files. A cheap content signal that feeds "debt density per
//! module" and the briefing. HEAD-derived, scans each HEAD blob once; mid+ tier.
//!
//! Markers are matched UPPERCASE only and as whole tokens (a non-alphanumeric on
//! each side), which is the convention and keeps prose ("todo list") and
//! identifiers (`FIXMES`) from matching. git is the oracle: a row must be a marker
//! really present at that line of the HEAD blob.

use std::path::Path;

use anyhow::Result;

/// A marker before file_id assignment (path-keyed; stitched in `main`).
pub struct MarkerRaw {
    pub path: String,
    pub line: i32,
    pub marker: String,
    pub text: String,
}

const MARKERS: &[&str] = &["TODO", "FIXME", "HACK", "XXX", "NOTE"];
const TEXT_MAX: usize = 200;

fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(8000)].contains(&0)
}

/// Find whole-token uppercase markers in one line. Yields (marker, byte offset
/// just after the marker). A token boundary = start/end of line or a
/// non-ascii-alphanumeric neighbour.
fn markers_in_line(line: &str) -> Vec<(&'static str, usize)> {
    let b = line.as_bytes();
    let mut hits = Vec::new();
    for &m in MARKERS {
        let mk = m.as_bytes();
        let mut i = 0;
        while i + mk.len() <= b.len() {
            if &b[i..i + mk.len()] == mk {
                let before_ok = i == 0 || !b[i - 1].is_ascii_alphanumeric();
                let after = i + mk.len();
                let after_ok = after >= b.len() || !b[after].is_ascii_alphanumeric();
                if before_ok && after_ok {
                    hits.push((m, after));
                    i = after;
                    continue;
                }
            }
            i += 1;
        }
    }
    hits
}

/// Every marker in every HEAD text blob. Binary blobs and non-blob entries are
/// skipped. Parallelized over HEAD blobs (order-preserving, so identical output).
pub fn compute(repo_path: &Path) -> Result<Vec<MarkerRaw>> {
    crate::blame::par_head_blobs(repo_path, |path, data| {
        let mut out = Vec::new();
        if is_binary(data) {
            return out;
        }
        let text = String::from_utf8_lossy(data);
        for (idx, line) in text.lines().enumerate() {
            for (marker, after) in markers_in_line(line) {
                // Text after the marker: drop a leading ':' / '(' segment noise,
                // just trim and truncate for a readable snippet.
                let rest = line[after..].trim_start_matches([':', ' ', '\t']).trim_end();
                let snippet: String = rest.chars().take(TEXT_MAX).collect();
                out.push(MarkerRaw {
                    path: path.to_string(),
                    line: idx as i32 + 1,
                    marker: marker.to_string(),
                    text: snippet,
                });
            }
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::markers_in_line;

    #[test]
    fn matches_whole_uppercase_tokens_only() {
        assert_eq!(markers_in_line("// TODO: fix this").len(), 1);
        assert_eq!(markers_in_line("# FIXME later").len(), 1);
        assert_eq!(markers_in_line("x = FIXMES;").len(), 0); // not a whole token
        assert_eq!(markers_in_line("todo list in prose").len(), 0); // lowercase
        assert_eq!(markers_in_line("TODO and FIXME here").len(), 2);
        assert_eq!(markers_in_line("(XXX) urgent").len(), 1);
        assert_eq!(markers_in_line("noTODOhere").len(), 0);
    }
}
