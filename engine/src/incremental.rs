//!  — incremental update. On a new push, process only the delta:
//! walk commits in `(old_head, new_head]`, merge them with the previous index's
//! Parquet (read back to path-keyed form), and **re-blame only the files whose
//! content changed** — unchanged files keep their previous blame. The result is
//! identical to a full reindex (verified by the harness), for a fraction of the
//! cost (`incremental_cost_ratio`). Content-addressing is what makes this sound:
//! a file whose HEAD blob is unchanged had no delta commit touch it, so its
//! blame is unchanged.
//!
//! Fast-forward only (old_head is an ancestor of new_head). Force-push / rebase
//! falls back to a full reindex.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use anyhow::Result;

use crate::blame::{compute_blame_for, compute_blame_native_for, head_files, BlameRow};
use crate::ingest::{assemble, walk};
use crate::model::Ingested;
use crate::read::read_old;

/// True if `old` is an ancestor of `new` (or equal) — i.e. a fast-forward.
pub fn is_ancestor(repo_path: &Path, old: &str, new: &str) -> bool {
    Command::new("git")
        .args(["-C", &repo_path.to_string_lossy(), "merge-base", "--is-ancestor", old, new])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Paths whose content differs between `old` and `new` (added / modified / deleted).
fn changed_paths(repo_path: &Path, old: &str, new: &str) -> Result<HashSet<String>> {
    let out = Command::new("git")
        .args(["-C", &repo_path.to_string_lossy(), "diff", "--name-only", "-z", old, new])
        .output()?;
    Ok(String::from_utf8_lossy(&out.stdout).split('\0').filter(|s| !s.is_empty()).map(str::to_string).collect())
}

pub fn ingest_incremental(repo_path: &Path, out_dir: &Path, old_head: &str, new_head: &str, cache_dir: Option<std::path::PathBuf>) -> Result<Ingested> {
    let repo = gix::discover(repo_path)?;
    let new_id = gix::ObjectId::from_hex(new_head.as_bytes())?;
    let old_id = gix::ObjectId::from_hex(old_head.as_bytes())?;

    // Delta commits: reachable from new_head but not old_head.
    let new_parts = walk(&repo, vec![new_id], vec![old_id], cache_dir)?;
    let delta_commits = new_parts.commits.len();

    // Previous index, read back to path-keyed form, with the delta appended.
    let (mut parts, old_blame) = read_old(out_dir)?;
    parts.commits.extend(new_parts.commits);
    parts.messages.extend(new_parts.messages);
    parts.trailers.extend(new_parts.trailers);
    parts.parents.extend(new_parts.parents);
    parts.authors.extend(new_parts.authors);
    parts.changes.extend(new_parts.changes);
    parts.merge_changes.extend(new_parts.merge_changes);

    // Blame: reuse unchanged files, re-blame only changed/new ones.
    let new_files = head_files(repo_path)?;
    let new_set: HashSet<&str> = new_files.iter().map(String::as_str).collect();
    let changed = changed_paths(repo_path, old_head, new_head)?;
    let old_blame_paths: HashSet<&str> = old_blame.iter().map(|r| r.path.as_str()).collect();

    let reblame: Vec<String> = new_files
        .iter()
        .filter(|p| changed.contains(p.as_str()) || !old_blame_paths.contains(p.as_str()))
        .cloned()
        .collect();
    let reblame_set: HashSet<&str> = reblame.iter().map(String::as_str).collect();

    // Keep old blame for files that still exist at HEAD and weren't re-blamed.
    let mut blame: Vec<BlameRow> = old_blame
        .into_iter()
        .filter(|r| new_set.contains(r.path.as_str()) && !reblame_set.contains(r.path.as_str()))
        .collect();
    // Re-blame only the changed files, with the same implementation as the full
    // index (git-CLI by default; EOS_BLAME=native opts into the gitoxide path).
    if std::env::var("EOS_BLAME").as_deref() == Ok("native") {
        blame.extend(compute_blame_native_for(repo_path, &reblame)?);
    } else {
        blame.extend(compute_blame_for(repo_path, &reblame)?);
    }

    // L3 facts are cheap (~0.17 ms/blob), so recompute over the full HEAD rather
    // than carrying a delta — always correct, no stale-fact risk. Skipped at the
    // `basic` tier (ownership/GRAIL gating happens in `assemble`).
    let mid = crate::ingest::Level::from_env() >= crate::ingest::Level::Mid;
    let (symbols, refs) = if mid {
        crate::symbols::compute_l3(repo_path)?
    } else {
        (Vec::new(), Vec::new())
    };
    // Content-marker generated files (mid+) for the ownership/insights exclusion —
    // recomputed over the full HEAD like L3, so incremental matches full.
    let content_generated: HashSet<String> = if mid {
        crate::generated::compute_content(repo_path)?.into_iter().collect()
    } else {
        HashSet::new()
    };

    eprintln!(
        "  incremental: {} new commits · re-blamed {}/{} files (reused the rest) · {} symbols, {} refs",
        delta_commits, reblame.len(), new_files.len(), symbols.len(), refs.len()
    );
    Ok(assemble(parts, blame, symbols, refs, &content_generated))
}
