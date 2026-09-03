//!  — content-addressed per-blob facts (the cross-tenant moat's lexical
//! layer). One row per DISTINCT HEAD blob, keyed by its git blob SHA. Every fact is
//! a pure function of the blob's bytes, so the same content anywhere — any path,
//! commit, or tenant — yields the same row: extract once per unique blob, reuse
//! everywhere it appears. Within a repo this dedups blob instances across paths and
//! history; the payoff generalizes to cross-tenant sharing (~45% overlap, spikes 3+4).

use std::path::Path;

use anyhow::{Context, Result};
use rayon::prelude::*;

use crate::model::BlobFactRow;

/// git's line count: number of '\n', plus 1 for a final line with no trailing
/// newline. Empty blob → 0. (Same rule as blame's count_lines.)
fn count_lines(data: &[u8]) -> i64 {
    if data.is_empty() {
        return 0;
    }
    let nl = data.iter().filter(|&&b| b == b'\n').count() as i64;
    if *data.last().unwrap() == b'\n' { nl } else { nl + 1 }
}

fn is_binary(data: &[u8]) -> bool {
    data[..data.len().min(8000)].contains(&0)
}

/// Facts for each distinct blob sha. `shas` may contain duplicates and non-blob
/// object ids (submodule gitlinks point at commits) — only distinct, resolvable
/// BLOB objects produce a row. Reads each unique blob once, in parallel (one gix
/// handle per worker). Rows are sorted by sha for a deterministic, diff-stable table.
pub fn compute(repo_path: &Path, shas: &[String]) -> Result<Vec<BlobFactRow>> {
    let root = repo_path.to_path_buf();
    let mut unique: Vec<&String> = shas.iter().collect();
    unique.sort();
    unique.dedup();

    let opts: Vec<Option<BlobFactRow>> = unique
        .par_iter()
        .map_init(
            || gix::discover(&root).ok(),
            |repo, sha| {
                let repo = repo.as_ref()?;
                let oid = gix::ObjectId::from_hex(sha.as_bytes()).ok()?;
                let obj = repo.find_object(oid).ok()?;
                if obj.kind != gix::objs::Kind::Blob {
                    return None; // a gitlink/tree, not a blob
                }
                let data = &obj.data;
                Some(BlobFactRow {
                    blob_sha: (*sha).clone(),
                    size_bytes: data.len() as i64,
                    line_count: count_lines(data),
                    is_binary: is_binary(data),
                })
            },
        )
        .collect();

    let mut rows: Vec<BlobFactRow> = opts.into_iter().flatten().collect();
    rows.sort_by(|a, b| a.blob_sha.cmp(&b.blob_sha));
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::count_lines;

    #[test]
    fn line_count_matches_git_rule() {
        assert_eq!(count_lines(b""), 0);
        assert_eq!(count_lines(b"\n"), 1);
        assert_eq!(count_lines(b"a"), 1);
        assert_eq!(count_lines(b"a\nb\n"), 2);
        assert_eq!(count_lines(b"a\nb"), 2);
    }
}
