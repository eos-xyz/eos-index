//!  — tree diff with subtree-hash pruning.
//!  — line counts for each change, via git's own xdiff (`libgit2`).
//!
//! For each non-merge commit, diff its tree to its first parent's. Because git
//! trees are content-addressed, **if two subtree OIDs are equal the whole
//! subtree is unchanged — skip it** (O(changed·depth), not O(all files)).
//! Then, per changed blob, count added/removed lines with **git's exact xdiff**
//! (libgit2), so `numstat` matches `git --numstat` bit-for-bit. Binary blobs
//! report null counts, exactly like git.
//!
//! Rename detection (R) is ; here a rename is a delete + add.

use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};

use gix::objs::tree::EntryKind;
use gix::{ObjectId, Repository};

/// git treats a blob as binary if a NUL appears in its first 8000 bytes.
const BINARY_SCAN: usize = 8000;

pub struct Change {
    pub change_type: char, // A M D T R
    pub path: String,
    pub old_path: Option<String>, // Some for R (rename) — the previous path
    pub similarity: Option<i32>,  // 100 for exact renames, else None
    pub src_blob_sha: String,   // pre-image blob, or all-zero if absent
    pub dst_blob_sha: String,   // post-image blob, or all-zero if absent
    pub src_mode: String,       // pre-image git mode (octal), "000000" if absent
    pub dst_mode: String,       // post-image git mode (octal), "000000" if absent
    pub added_lines: Option<i32>,   // None for binary (git prints '-')
    pub removed_lines: Option<i32>,
    pub hunks: Vec<HunkRaw>,        // per-hunk +/- ranges (empty for binary/gitlink)
}

/// The 6-digit octal git mode of a tree entry () — the `100644`,
/// `100755` (executable), `120000` (symlink), `160000` (gitlink) that live in the
/// tree and are otherwise lost. `000000` is the git convention for an absent side.
const MODE_ABSENT: &str = "000000";
fn mode_str(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Tree => "040000",
        EntryKind::Blob => "100644",
        EntryKind::BlobExecutable => "100755",
        EntryKind::Link => "120000",
        EntryKind::Commit => "160000",
    }
}

/// One diff hunk () — a contiguous changed region, as
/// `@@ -old_start,old_lines +new_start,new_lines @@`. Computed with zero context
/// so `sum(new_lines)` = added lines and `sum(old_lines)` = removed lines.
pub struct HunkRaw {
    pub old_start: i32,
    pub old_lines: i32,
    pub new_start: i32,
    pub new_lines: i32,
}

#[derive(Default)]
pub struct Stats {
    pub tree_loads: usize, // trees actually read — the pruning shows here
}

struct Ent {
    oid: ObjectId,
    kind: EntryKind,
    is_tree: bool,
}

fn load(repo: &Repository, id: &ObjectId, stats: &mut Stats) -> Result<BTreeMap<Vec<u8>, Ent>> {
    stats.tree_loads += 1;
    let tree = repo.find_tree(*id)?;
    let tref = tree.decode()?;
    let mut m = BTreeMap::new();
    for e in tref.entries.iter() {
        m.insert(
            e.filename.to_vec(),
            Ent { oid: e.oid.to_owned(), kind: e.mode.kind(), is_tree: e.mode.is_tree() },
        );
    }
    Ok(m)
}

fn type_class(k: EntryKind) -> u8 {
    match k {
        EntryKind::Tree => 0,
        EntryKind::Blob | EntryKind::BlobExecutable => 1, // exec-bit flip stays M
        EntryKind::Link => 2,
        EntryKind::Commit => 3,
    }
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(BINARY_SCAN)].contains(&0)
}

fn blob_bytes(repo: &Repository, oid: Option<&ObjectId>) -> Result<Vec<u8>> {
    match oid {
        Some(id) => Ok(repo.find_object(*id)?.data.clone()),
        None => Ok(Vec::new()),
    }
}

/// Added/removed line counts via git's own xdiff (libgit2), so they match
/// `git --numstat` exactly. None/None when either side is binary (git prints '-').
/// Line counts + per-hunk ranges from one blob diff. Binary → (None, None) and no
/// hunks. The hunks are free: the same `git2::Patch` that gives numstat gives them.
pub fn line_diff(
    repo: &Repository,
    old: Option<&ObjectId>,
    new: Option<&ObjectId>,
) -> Result<((Option<i32>, Option<i32>), Vec<HunkRaw>)> {
    let old_bytes = blob_bytes(repo, old)?;
    let new_bytes = blob_bytes(repo, new)?;
    if is_binary(&old_bytes) || is_binary(&new_bytes) {
        return Ok(((None, None), Vec::new()));
    }
    let (add, del, hunks) = xdiff(&old_bytes, &new_bytes)?;
    Ok(((Some(add), Some(del)), hunks))
}

/// Counts (bit-exact vs `git --numstat`, context-independent) plus per-hunk ranges
/// computed at ZERO context, so each hunk is the minimal changed range (like
/// `git diff -U0`) and `sum(new_lines)`=added, `sum(old_lines)`=removed.
pub fn xdiff(old: &[u8], new: &[u8]) -> Result<(i32, i32, Vec<HunkRaw>)> {
    let mut opts = git2::DiffOptions::new();
    opts.indent_heuristic(true).context_lines(0);
    let patch = git2::Patch::from_buffers(old, None, new, None, Some(&mut opts))?;
    let (_ctx, add, del) = patch.line_stats()?;
    let mut hunks = Vec::with_capacity(patch.num_hunks());
    for i in 0..patch.num_hunks() {
        let (h, _) = patch.hunk(i)?;
        hunks.push(HunkRaw {
            old_start: h.old_start() as i32,
            old_lines: h.old_lines() as i32,
            new_start: h.new_start() as i32,
            new_lines: h.new_lines() as i32,
        });
    }
    Ok((add as i32, del as i32, hunks))
}

pub fn diff_commit(
    repo: &Repository,
    old_tree: Option<ObjectId>,
    new_tree: ObjectId,
    zero: &str,
    stats: &mut Stats,
) -> Result<Vec<Change>> {
    let mut out = Vec::new();
    diff_trees(repo, old_tree.as_ref(), Some(&new_tree), &[], zero, stats, &mut out)?;
    Ok(detect_exact_renames(out))
}

///  — exact renames: a delete and an add of the **same blob** in one
/// commit are a rename (100% similarity), exactly `git -M100%`. Inexact
/// (edited) renames are ; they stay as delete + add here.
fn detect_exact_renames(changes: Vec<Change>) -> Vec<Change> {
    use std::collections::HashMap;
    let mut dels: HashMap<String, Vec<Change>> = HashMap::new();
    let mut adds: HashMap<String, Vec<Change>> = HashMap::new();
    let mut out: Vec<Change> = Vec::new();
    for ch in changes {
        match ch.change_type {
            'D' => dels.entry(ch.src_blob_sha.clone()).or_default().push(ch),
            'A' => adds.entry(ch.dst_blob_sha.clone()).or_default().push(ch),
            _ => out.push(ch),
        }
    }

    // Deterministic pairing: sort blobs, and within a blob sort by path, so a
    // duplicate-content ambiguity resolves the same way every run.
    let mut blobs: Vec<String> = dels.keys().cloned().collect();
    blobs.sort();
    for sha in blobs {
        let mut ds = dels.remove(&sha).unwrap();
        match adds.remove(&sha) {
            Some(mut as_) => {
                ds.sort_by(|a, b| a.path.cmp(&b.path));
                as_.sort_by(|a, b| a.path.cmp(&b.path));
                let n = ds.len().min(as_.len());
                for i in 0..n {
                    out.push(Change {
                        change_type: 'R',
                        path: as_[i].path.clone(),
                        old_path: Some(ds[i].path.clone()),
                        similarity: Some(100),
                        src_blob_sha: sha.clone(),
                        dst_blob_sha: sha.clone(),
                        src_mode: ds[i].src_mode.clone(), // mode of the deleted (old) entry
                        dst_mode: as_[i].dst_mode.clone(), // mode of the added (new) entry
                        added_lines: Some(0), // pure rename: no line changes
                        removed_lines: Some(0),
                        hunks: Vec::new(),
                    });
                }
                out.extend(ds.into_iter().skip(n)); // unpaired deletes stay D
                out.extend(as_.into_iter().skip(n)); // unpaired adds stay A
            }
            None => out.extend(ds), // no matching add — all stay D
        }
    }
    for (_sha, as_) in adds {
        out.extend(as_); // adds with no matching delete stay A
    }
    out
}

fn path_str(prefix: &[u8], name: &[u8]) -> (Vec<u8>, String) {
    let mut v = prefix.to_vec();
    if !v.is_empty() {
        v.push(b'/');
    }
    v.extend_from_slice(name);
    let s = String::from_utf8_lossy(&v).into_owned();
    (v, s)
}

/// A gitlink (submodule) entry — a commit id that lives in another repository,
/// so it has no blob bytes here. git records the pointer change with no line
/// counts; reading it as a blob would fail ("object not found").
fn is_gitlink(e: &Ent) -> bool {
    matches!(e.kind, EntryKind::Commit)
}

fn add_side(repo: &Repository, e: &Ent, path: &[u8], zero: &str, stats: &mut Stats, out: &mut Vec<Change>) -> Result<()> {
    if e.is_tree {
        diff_trees(repo, None, Some(&e.oid), path, zero, stats, out)
    } else {
        let ((added, removed), hunks) = if is_gitlink(e) { ((None, None), Vec::new()) } else { line_diff(repo, None, Some(&e.oid))? };
        out.push(Change {
            change_type: 'A',
            path: String::from_utf8_lossy(path).into_owned(),
            old_path: None,
            similarity: None,
            src_blob_sha: zero.to_string(),
            dst_blob_sha: e.oid.to_string(),
            src_mode: MODE_ABSENT.to_string(),
            dst_mode: mode_str(e.kind).to_string(),
            added_lines: added,
            removed_lines: removed,
            hunks,
        });
        Ok(())
    }
}

fn del_side(repo: &Repository, e: &Ent, path: &[u8], zero: &str, stats: &mut Stats, out: &mut Vec<Change>) -> Result<()> {
    if e.is_tree {
        diff_trees(repo, Some(&e.oid), None, path, zero, stats, out)
    } else {
        let ((added, removed), hunks) = if is_gitlink(e) { ((None, None), Vec::new()) } else { line_diff(repo, Some(&e.oid), None)? };
        out.push(Change {
            change_type: 'D',
            path: String::from_utf8_lossy(path).into_owned(),
            old_path: None,
            similarity: None,
            src_blob_sha: e.oid.to_string(),
            dst_blob_sha: zero.to_string(),
            src_mode: mode_str(e.kind).to_string(),
            dst_mode: MODE_ABSENT.to_string(),
            added_lines: added,
            removed_lines: removed,
            hunks,
        });
        Ok(())
    }
}

fn diff_trees(
    repo: &Repository,
    old: Option<&ObjectId>,
    new: Option<&ObjectId>,
    prefix: &[u8],
    zero: &str,
    stats: &mut Stats,
    out: &mut Vec<Change>,
) -> Result<()> {
    let om = match old {
        Some(id) => load(repo, id, stats)?,
        None => BTreeMap::new(),
    };
    let nm = match new {
        Some(id) => load(repo, id, stats)?,
        None => BTreeMap::new(),
    };

    let names: BTreeSet<&Vec<u8>> = om.keys().chain(nm.keys()).collect();
    for name in names {
        let o = om.get(name);
        let n = nm.get(name);
        let (path_bytes, path) = path_str(prefix, name);
        match (o, n) {
            (None, Some(n)) => add_side(repo, n, &path_bytes, zero, stats, out)?,
            (Some(o), None) => del_side(repo, o, &path_bytes, zero, stats, out)?,
            (Some(o), Some(n)) => {
                if o.oid == n.oid && o.kind == n.kind {
                    continue; // subtree/blob unchanged — PRUNE
                }
                if o.is_tree && n.is_tree {
                    diff_trees(repo, Some(&o.oid), Some(&n.oid), &path_bytes, zero, stats, out)?;
                } else if o.is_tree != n.is_tree {
                    // dir <-> file at the same name: delete one side, add the other.
                    del_side(repo, o, &path_bytes, zero, stats, out)?;
                    add_side(repo, n, &path_bytes, zero, stats, out)?;
                } else {
                    // both non-tree (blobs, symlinks, or gitlinks)
                    let ct = if type_class(o.kind) != type_class(n.kind) { 'T' } else { 'M' };
                    // A submodule pointer change has no blob bytes on either side.
                    let ((added, removed), hunks) = if is_gitlink(o) || is_gitlink(n) {
                        ((None, None), Vec::new())
                    } else {
                        line_diff(repo, Some(&o.oid), Some(&n.oid))?
                    };
                    out.push(Change {
                        change_type: ct,
                        path,
                        old_path: None,
                        similarity: None,
                        src_blob_sha: o.oid.to_string(),
                        dst_blob_sha: n.oid.to_string(),
                        src_mode: mode_str(o.kind).to_string(),
                        dst_mode: mode_str(n.kind).to_string(),
                        added_lines: added,
                        removed_lines: removed,
                        hunks,
                    });
                }
            }
            (None, None) => unreachable!(),
        }
    }
    Ok(())
}
