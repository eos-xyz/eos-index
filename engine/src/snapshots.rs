//! .7b — historical blame snapshots.
//!
//! HEAD blame () answers "who owns each line *now*". A snapshot answers
//! the same question at a point in the past — the tree as it stood at a tag, a
//! release, or any chosen revision — so ownership, bus factor and code age can be
//! tracked *over time*, not just at the tip.
//!
//! Re-blaming every commit's whole tree is O(commits × files) and infeasible on a
//! large history, so snapshots are a *bounded, opt-in* set of revisions, not every
//! commit. Selection is driven by `EOS_SNAPSHOTS`:
//!
//!   (unset)                  no snapshots — zero extra work (the default).
//!   EOS_SNAPSHOTS=tags       one snapshot per tag (releases), most-recent first,
//!                            capped at EOS_SNAPSHOTS_MAX (default 20).
//!   EOS_SNAPSHOTS=tags:N     as above, capped at N.
//!   EOS_SNAPSHOTS=a,b,c      snapshot exactly these revisions (tags/shas/branches).
//!
//! Each snapshot is an exact `git blame <rev>` over that revision's tree, so git
//! remains the oracle (the bench verifies a sample against `git blame <sha>`).

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

use crate::blame::{compute_blame_rev_for, files_at};
use crate::model::SnapshotBlameRow;

const DEFAULT_MAX: usize = 20;

/// A resolved snapshot target: the label the user asked for and the commit it
/// peels to.
struct Target {
    label: String,
    sha: String,
}

/// Parse `EOS_SNAPSHOTS` and resolve it to concrete `(label, commit-sha)` targets.
/// Returns an empty vec when snapshots aren't requested. Never errors on a bad
/// individual ref — it warns and skips, so one stale tag can't fail the index.
fn select(repo_path: &Path, default_tags: bool) -> Result<Vec<Target>> {
    let spec = match std::env::var("EOS_SNAPSHOTS") {
        Ok(s) if !s.trim().is_empty() => s,
        // Unset: the `high` tier defaults to tags; otherwise no snapshots.
        _ => {
            return Ok(if default_tags {
                select_tags(repo_path, DEFAULT_MAX)
            } else {
                Vec::new()
            })
        }
    };
    let spec = spec.trim();

    if spec == "tags" || spec.starts_with("tags:") {
        let max = spec
            .strip_prefix("tags:")
            .and_then(|n| n.trim().parse::<usize>().ok())
            .unwrap_or_else(|| {
                std::env::var("EOS_SNAPSHOTS_MAX").ok().and_then(|n| n.parse().ok()).unwrap_or(DEFAULT_MAX)
            });
        return Ok(select_tags(repo_path, max));
    }

    // Explicit comma-separated revision list. Preserve the user's order; dedup by
    // resolved commit so `main` and its sha don't produce two identical snapshots.
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for raw in spec.split(',') {
        let label = raw.trim();
        if label.is_empty() {
            continue;
        }
        match resolve_commit(repo_path, label) {
            Some(sha) if seen.insert(sha.clone()) => out.push(Target { label: label.to_string(), sha }),
            Some(_) => {} // duplicate commit — skip silently
            None => eprintln!("  snapshots: skipping '{label}' — not a resolvable commit"),
        }
    }
    Ok(out)
}

/// Peel `rev` to a commit sha, or `None` if it isn't one (a tag on a tree/blob, a
/// typo, a deleted ref). `<rev>^{{commit}}` dereferences annotated tags and fails
/// cleanly on non-commits.
fn resolve_commit(repo_path: &Path, rev: &str) -> Option<String> {
    let root = repo_path.to_string_lossy().to_string();
    let out = Command::new("git")
        .args(["-C", &root, "rev-parse", "--verify", "-q", &format!("{rev}^{{commit}}")])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit())).then_some(sha)
}

/// All tags that point at a commit, most-recent first, deduped by commit and
/// capped at `max`. Uses one `for-each-ref` pass: `*objectname`/`*objecttype` are
/// the peeled (dereferenced) object for annotated tags, empty for lightweight
/// ones, so `peeled-or-direct` gives the commit for either kind.
fn select_tags(repo_path: &Path, max: usize) -> Vec<Target> {
    let root = repo_path.to_string_lossy().to_string();
    let fmt = "%(refname:short)\t%(objectname)\t%(objecttype)\t%(*objectname)\t%(*objecttype)";
    let out = match Command::new("git")
        .args(["-C", &root, "for-each-ref", "--sort=-creatordate", &format!("--format={fmt}"), "refs/tags"])
        .output()
    {
        Ok(o) if o.status.success() => o.stdout,
        _ => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&out);

    let mut targets = Vec::new();
    let mut seen = BTreeSet::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 5 {
            continue;
        }
        let (name, direct_sha, direct_type, peeled_sha, peeled_type) = (f[0], f[1], f[2], f[3], f[4]);
        // Peeled fields are set for annotated tags; lightweight tags carry the
        // commit directly. Keep only tags that resolve to a commit.
        let (sha, ty) = if !peeled_sha.is_empty() { (peeled_sha, peeled_type) } else { (direct_sha, direct_type) };
        if ty != "commit" || sha.len() != 40 {
            continue;
        }
        if seen.insert(sha.to_string()) {
            targets.push(Target { label: name.to_string(), sha: sha.to_string() });
        }
    }

    if targets.len() > max {
        let dropped = targets.len() - max;
        eprintln!(
            "  snapshots: {} tags resolve to commits; capping at the {} most recent ({} older skipped — raise EOS_SNAPSHOTS_MAX)",
            targets.len(), max, dropped
        );
        targets.truncate(max);
    }
    targets
}

/// Build historical blame for every requested snapshot. One exact `git blame`
/// pass per snapshot (files fanned out across cores inside `compute_blame_rev_for`).
/// Snapshots run sequentially to keep peak memory bounded on large repos.
pub fn compute(repo_path: &Path, default_tags: bool) -> Result<Vec<SnapshotBlameRow>> {
    let targets = select(repo_path, default_tags)?;
    if targets.is_empty() {
        return Ok(Vec::new());
    }
    eprintln!("  snapshots: blaming {} historical snapshot(s)", targets.len());

    let mut rows = Vec::new();
    for t in &targets {
        let files = files_at(repo_path, &t.sha).with_context(|| format!("ls-tree {}", t.sha))?;
        let blamed = compute_blame_rev_for(repo_path, &t.sha, &files)?;
        eprintln!(
            "    {} ({}): {} files, {} lines",
            t.label, &t.sha[..8.min(t.sha.len())], files.len(), blamed.len()
        );
        rows.reserve(blamed.len());
        for b in blamed {
            rows.push(SnapshotBlameRow {
                snapshot_ref: t.label.clone(),
                snapshot_sha: t.sha.clone(),
                path: b.path,
                line_number: b.line_number,
                commit_sha: b.commit_sha,
            });
        }
    }
    Ok(rows)
}
