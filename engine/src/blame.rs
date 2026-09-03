//!  — line provenance (blame), matching `git blame` exactly.
//!
//! Blame is the one order-dependent fact, and matching `git blame` means
//! matching git's intricate merge parent-selection and diff-boundary heuristics.
//! libgit2's blame is git-compatible but ~8× slower than git's own (its port
//! lacks git's optimizations). Since git's blame is the reference *and* fast, we
//! shell out to `git blame` and fan out across files with rayon.
//!
//! **Fast path (single-add files):** a file added in exactly one commit and never
//! touched since has a trivial blame — every line is that add commit — so we skip
//! `git blame` for it entirely and just read the HEAD blob and count lines (gix,
//! in parallel). On a typical repo that is ~half the files and ~half the blame
//! time (it is all `git blame` process startup, there being no history to walk),
//! so this roughly halves a cold index. The result is identical to `git blame`
//! (verified on every single-add file), and the `blame_sample` oracle still checks
//! the whole table against `git blame`.
//!
//! Verified ~100% agreement with `git blame` (the slow path *is* git blame; the
//! fast path is a proven-equivalent shortcut).

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use rayon::prelude::*;

pub struct BlameRow {
    pub path: String,
    pub line_number: i32,
    pub commit_sha: String,
}

/// Whether a `git blame --line-porcelain` line is a per-line header:
/// `<40-hex> <orig-line> <final-line>[ <num-lines>]`. Its first token is the sha.
fn header_sha(line: &str) -> Option<&str> {
    let b = line.as_bytes();
    if b.len() >= 42
        && b[..40].iter().all(u8::is_ascii_hexdigit)
        && b[40] == b' '
        && b[41].is_ascii_digit()
    {
        Some(&line[..40])
    } else {
        None
    }
}

/// Every blob path under a rev's tree (NUL-delimited for odd names). `rev` is any
/// git revision (`HEAD`, a tag, a sha) — snapshots (.7b) blame historical
/// trees, not just HEAD.
pub fn files_at(repo_path: &Path, rev: &str) -> Result<Vec<String>> {
    let root = repo_path.to_string_lossy().to_string();
    let out = Command::new("git")
        .args(["-C", &root, "ls-tree", "-r", rev, "--name-only", "-z"])
        .output()
        .context("git ls-tree")?;
    Ok(String::from_utf8_lossy(&out.stdout).split('\0').filter(|s| !s.is_empty()).map(str::to_string).collect())
}

/// Every blob path under HEAD (NUL-delimited for odd names).
pub fn head_files(repo_path: &Path) -> Result<Vec<String>> {
    files_at(repo_path, "HEAD")
}

/// Run `scan` over every HEAD blob's bytes IN PARALLEL (one gix handle per rayon
/// worker, like the blame fast path), flattening the per-blob results in HEAD-file
/// order — so the output is identical to a serial scan, just faster. Non-blob
/// entries (gitlinks/trees) are skipped; `scan` gets (path, blob bytes) and decides
/// what to emit (including any binary check). The content scanners — markers,
/// secrets, generated-file markers — share this instead of each looping serially.
pub fn par_head_blobs<T, F>(repo_path: &Path, scan: F) -> Result<Vec<T>>
where
    T: Send,
    F: Fn(&str, &[u8]) -> Vec<T> + Sync,
{
    let root = repo_path.to_path_buf();
    let head_tree_id = {
        let repo = gix::discover(&root).context("open repo (gix)")?;
        let commit = repo.head_commit().context("HEAD")?;
        commit.tree_id().context("HEAD tree")?.detach()
    };
    let files = head_files(repo_path)?;
    let chunks: Vec<Vec<T>> = files
        .par_iter()
        .map_init(
            || gix::discover(&root).ok(),
            |repo, path| {
                let Some(repo) = repo.as_ref() else { return Vec::new() };
                let Ok(tree) = repo.find_tree(head_tree_id) else { return Vec::new() };
                let Some(entry) = tree.lookup_entry_by_path(Path::new(path)).ok().flatten() else { return Vec::new() };
                if !entry.mode().is_blob() {
                    return Vec::new();
                }
                let Ok(obj) = repo.find_object(entry.object_id()) else { return Vec::new() };
                scan(path, &obj.data)
            },
        )
        .collect();
    Ok(chunks.into_iter().flatten().collect())
}

/// git's line count for a file: number of '\n', plus 1 for a final line with no
/// trailing newline. Empty file → 0. This is exactly how many lines `git blame`
/// emits for the file (verified on every single-add file of eos-monorepo).
fn count_lines(data: &[u8]) -> usize {
    if data.is_empty() {
        return 0;
    }
    let nl = data.iter().filter(|&&b| b == b'\n').count();
    if *data.last().unwrap() == b'\n' { nl } else { nl + 1 }
}

/// Blame every current (HEAD) file, with a **fast path for "single-add" files**.
///
/// A file that was ADDED in exactly one commit and never touched since has a
/// trivial blame: every line originates in that one add commit. On a typical repo
/// that is ~half the files, and blaming them costs ~half the total blame time —
/// all in `git blame` process startup, since there is no history to walk. So for
/// those we read the HEAD blob directly (gix, in parallel) and count its lines —
/// no subprocess. `single_add` maps such a HEAD path to its add-commit sha.
/// Everything else (and any path that isn't a plain blob, e.g. a submodule
/// gitlink) goes through `git blame`, the exact reference.
///
/// The result is identical to blaming every file with `git blame` — the fast path
/// is a proven-equivalent shortcut, and the `blame_sample` oracle still checks the
/// whole table against `git blame`.
pub fn compute_blame(repo_path: &Path, single_add: &HashMap<String, String>) -> Result<Vec<BlameRow>> {
    let files = head_files(repo_path)?;
    let (fast, mut slow): (Vec<&String>, Vec<&String>) =
        files.iter().partition(|f| single_add.contains_key(*f));

    // Fast path: read each single-add HEAD blob, attribute every line to its add
    // commit. Each worker resolves the HEAD tree from its own repo handle. A path
    // that isn't a plain blob returns None → it falls back to `git blame`.
    let root = repo_path.to_path_buf();
    let head_tree_id = {
        let repo = gix::discover(&root).context("open repo (gix)")?;
        let commit = repo.head_commit().context("HEAD")?;
        let tree_id = commit.tree_id().context("HEAD tree")?.detach();
        tree_id
    };
    let outcomes: Vec<(Vec<BlameRow>, Option<String>)> = fast
        .par_iter()
        .map_init(
            || gix::discover(&root).ok(),
            |repo, path| {
                let commit = &single_add[*path];
                match repo.as_ref().and_then(|r| blob_line_count(r, head_tree_id, path)) {
                    Some(n) => {
                        let rows = (0..n)
                            .map(|i| BlameRow { path: (*path).clone(), line_number: i as i32 + 1, commit_sha: commit.clone() })
                            .collect();
                        (rows, None)
                    }
                    None => (Vec::new(), Some((*path).clone())), // not a plain blob → fall back
                }
            },
        )
        .collect();

    let mut rows: Vec<BlameRow> = Vec::new();
    let mut fallback: Vec<String> = Vec::new();
    for (r, fb) in outcomes {
        rows.extend(r);
        if let Some(p) = fb {
            fallback.push(p);
        }
    }
    // Blame the remainder (multi-touch files + any fast-path fallback) with git.
    slow.extend(fallback.iter());
    let slow_owned: Vec<String> = slow.iter().map(|s| (*s).clone()).collect();
    rows.extend(compute_blame_rev_for(repo_path, "HEAD", &slow_owned)?);
    Ok(rows)
}

/// Line count of the HEAD blob at `path`, or None if it isn't a plain blob
/// (a submodule gitlink or a missing/odd entry) — those fall back to `git blame`.
fn blob_line_count(repo: &gix::Repository, head_tree_id: gix::ObjectId, path: &str) -> Option<usize> {
    let tree = repo.find_tree(head_tree_id).ok()?;
    let entry = tree.lookup_entry_by_path(Path::new(path)).ok()??;
    if !entry.mode().is_blob() {
        return None; // gitlink/tree — git blame handles (or fails on) it
    }
    let obj = repo.find_object(entry.object_id()).ok()?;
    Some(count_lines(&obj.data))
}

/// Native blame via gitoxide (`gix::blame`) for every HEAD file — no per-file
/// subprocess, in-process over one object store. EXPERIMENTAL opt-in
/// (`EOS_BLAME=native`): identical coverage, but measured worse than git-CLI at
/// scale — 99.7% agreement on eos-monorepo but only 95% on vscode (116k commits),
/// 2.3× slower and 3× the memory there (gitoxide's diff/rename line-matching
/// diverges from git's and compounds on merge-heavy history). git-CLI is the
/// default and recommendation; this is kept for git-binary-free small-repo use
/// and as a reference. The PR's true "blame as a fold" is a separate, larger job.
pub fn compute_blame_native(repo_path: &Path) -> Result<Vec<BlameRow>> {
    compute_blame_native_for(repo_path, &head_files(repo_path)?)
}

/// Native blame for a specific set of HEAD files (used by the incremental path so
/// a push re-blames only the changed files, in-process).
pub fn compute_blame_native_for(repo_path: &Path, files: &[String]) -> Result<Vec<BlameRow>> {
    use gix::bstr::BStr;
    use rayon::prelude::*;

    let repo = gix::discover(repo_path).context("open repo (gix)")?;
    let head = repo.head_commit().context("HEAD")?.id().detach();
    let root = repo_path.to_path_buf();

    // git blame follows the file through its own renames; enable rewrite tracking
    // to match. Each rayon worker opens its own repo (Repository isn't Sync).
    let rows: Vec<BlameRow> = files
        .par_iter()
        .map_init(
            || gix::discover(&root).expect("open repo (gix, worker)"),
            |repo, path| {
                // Match `git blame`'s defaults: Myers diff (git's default algorithm)
                // and rename tracking (git blame follows a file through its renames).
                let opts = gix::repository::blame_file::Options {
                    diff_algorithm: Some(gix::diff::blob::Algorithm::Myers),
                    rewrites: Some(gix::diff::Rewrites::default()),
                    ..Default::default()
                };
                let mut out = Vec::new();
                let outcome = match repo.blame_file(BStr::new(path.as_bytes()), head, opts) {
                    Ok(o) => o,
                    Err(_) => return out, // unblameable (submodule, binary, gone) — skip
                };
                for e in &outcome.entries {
                    let sha = e.commit_id.to_string();
                    for i in 0..e.len.get() {
                        out.push(BlameRow {
                            path: path.clone(),
                            line_number: (e.start_in_blamed_file + i) as i32 + 1,
                            commit_sha: sha.clone(),
                        });
                    }
                }
                out
            },
        )
        .flatten()
        .collect();
    Ok(rows)
}

/// Blame a specific set of HEAD files (used by the incremental path).
pub fn compute_blame_for(repo_path: &Path, files: &[String]) -> Result<Vec<BlameRow>> {
    compute_blame_rev_for(repo_path, "HEAD", files)
}

/// Blame a set of files as they existed at `rev` (any revision — `HEAD`, a tag, a
/// sha), in parallel across files. The line numbers and origin shas are relative
/// to that snapshot's tree, so a file deleted or renamed after `rev` still blames
/// correctly at `rev`. Underpins historical blame snapshots (.7b).
pub fn compute_blame_rev_for(repo_path: &Path, rev: &str, files: &[String]) -> Result<Vec<BlameRow>> {
    let root = repo_path.to_string_lossy().to_string();
    let rows: Vec<BlameRow> = files
        .par_iter()
        .flat_map_iter(|path| {
            let mut out = Vec::new();
            let blame = match Command::new("git")
                // `--porcelain` (not `--line-porcelain`): both emit one
                // `<sha> <orig> <final>` header per line — the only thing we parse —
                // but `--porcelain` prints each commit's author/date/summary block
                // ONCE, not per line, so a file dominated by few commits produces
                // ~5-6× less output. Same blame, same per-line shas (verified),
                // far less pipe I/O and parsing — the win compounds at scale.
                .args(["-C", &root, "blame", "--porcelain", rev, "--", path])
                .output()
            {
                Ok(o) if o.status.success() => o.stdout,
                _ => return out.into_iter(), // unblameable (submodule, binary, gone) — skip
            };
            let text = String::from_utf8_lossy(&blame);
            let mut line = 1i32;
            for l in text.lines() {
                if let Some(sha) = header_sha(l) {
                    out.push(BlameRow { path: path.clone(), line_number: line, commit_sha: sha.to_string() });
                    line += 1;
                }
            }
            out.into_iter()
        })
        .collect();
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::count_lines;

    #[test]
    fn line_count_matches_git_rule() {
        // git blame emits one line per '\n'-terminated segment, plus one for a
        // final segment with no trailing newline. Empty file → 0.
        assert_eq!(count_lines(b""), 0);
        assert_eq!(count_lines(b"\n"), 1); // one empty line
        assert_eq!(count_lines(b"a"), 1); // no trailing newline
        assert_eq!(count_lines(b"a\n"), 1);
        assert_eq!(count_lines(b"a\nb"), 2); // final line, no newline
        assert_eq!(count_lines(b"a\nb\n"), 2);
        assert_eq!(count_lines(b"a\r\nb\r\n"), 2); // CRLF: count '\n'
        assert_eq!(count_lines(b"\n\n\n"), 3); // three empty lines
        // binary bytes still split on '\n' — git blame blames any file as lines.
        assert_eq!(count_lines(b"\x00\x01\n\x02"), 2);
    }
}
