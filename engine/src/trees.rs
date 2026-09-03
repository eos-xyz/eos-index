//!  — historical tree objects (the lossless directory layer). git stores
//! the file tree at every commit as a graph of content-addressed tree objects; a
//! commit that changes one file re-uses every unchanged subtree's sha, so the set
//! of DISTINCT trees across all history is far smaller than commits × directories.
//! We store each distinct tree's direct entries once (`tree_objects`) plus each
//! commit's root (`commit_trees`), so the full file list at ANY commit is a lookup
//! + recursive expansion — no git object store needed. Bounded and opt-in
//! (EOS_TREES, on by default at `high`); git IS the oracle (`git ls-tree -r`).

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result};
use gix::objs::tree::EntryKind;
use gix::ObjectId;

use crate::model::{CommitTreeRow, TreeObjectRow};

/// git mode string + our entry_type label for a tree entry kind.
fn mode_and_type(kind: EntryKind) -> (&'static str, &'static str) {
    match kind {
        EntryKind::Tree => ("040000", "tree"),
        EntryKind::Blob => ("100644", "blob"),
        EntryKind::BlobExecutable => ("100755", "executable"),
        EntryKind::Link => ("120000", "symlink"),
        EntryKind::Commit => ("160000", "submodule"),
    }
}

/// Each commit's root tree, and the direct entries of every distinct tree object
/// reachable from those roots (deduped by tree sha). `commit_shas` is the indexed
/// commit set.
pub fn compute(repo_path: &Path, commit_shas: &[String]) -> Result<(Vec<CommitTreeRow>, Vec<TreeObjectRow>)> {
    let repo = gix::discover(repo_path).context("open repo (gix)")?;

    let mut commit_trees = Vec::with_capacity(commit_shas.len());
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut queue: Vec<ObjectId> = Vec::new();

    for sha in commit_shas {
        let Ok(oid) = ObjectId::from_hex(sha.as_bytes()) else { continue };
        let Ok(commit) = repo.find_commit(oid) else { continue };
        let Ok(tree_id) = commit.tree_id() else { continue };
        let root = tree_id.detach();
        commit_trees.push(CommitTreeRow { commit_sha: sha.clone(), root_tree_sha: root.to_string() });
        if seen.insert(root) {
            queue.push(root);
        }
    }

    // BFS over distinct tree objects; emit each tree's direct entries, enqueue its
    // subtrees. `seen` dedups so a shared subtree is walked (and stored) once.
    let mut tree_objects = Vec::new();
    let mut i = 0;
    while i < queue.len() {
        let toid = queue[i];
        i += 1;
        let Ok(tree) = repo.find_tree(toid) else { continue };
        let Ok(decoded) = tree.decode() else { continue };
        let tree_hex = toid.to_string();
        for (seq, e) in decoded.entries.iter().enumerate() {
            let (mode, entry_type) = mode_and_type(e.mode.kind());
            let child = e.oid.to_owned();
            tree_objects.push(TreeObjectRow {
                tree_sha: tree_hex.clone(),
                seq: seq as i32,
                name: String::from_utf8_lossy(e.filename).into_owned(),
                mode: mode.to_string(),
                entry_type: entry_type.to_string(),
                entry_sha: child.to_string(),
            });
            if e.mode.is_tree() && seen.insert(child) {
                queue.push(child);
            }
        }
    }

    Ok((commit_trees, tree_objects))
}
