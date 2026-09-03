//!  — the HEAD tree with git modes (permissions, executable bit,
//! symlinks, submodules) that the path/blob tables don't capture. One
//! `git ls-tree -r -l HEAD` pass; file_ids are stitched in by the caller.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

/// A raw HEAD tree entry before file_id assignment.
pub struct TreeRaw {
    pub path: String,
    pub mode: String,
    pub entry_type: String,
    pub blob_sha: String,
    pub size: Option<i64>,
}

fn entry_type(mode: &str) -> &'static str {
    match mode {
        "100755" => "executable",
        "120000" => "symlink",
        "160000" => "submodule",
        _ => "blob",
    }
}

/// Every leaf of the HEAD tree (blobs + submodule gitlinks; `-r` doesn't emit
/// directories). NUL-delimited for odd names.
pub fn head_entries(repo_path: &Path) -> Result<Vec<TreeRaw>> {
    let root = repo_path.to_string_lossy().to_string();
    // "<mode> <type> <sha> <size>\t<path>" — `-l` adds the size, `-z` NUL-delimits.
    let out = Command::new("git")
        .args(["-C", &root, "ls-tree", "-r", "-l", "-z", "HEAD"])
        .output()
        .context("git ls-tree -r -l HEAD")?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut rows = Vec::new();
    for entry in text.split('\0').filter(|s| !s.is_empty()) {
        // meta is whitespace-separated; the path follows a TAB.
        let (meta, path) = match entry.split_once('\t') {
            Some(x) => x,
            None => continue,
        };
        let cols: Vec<&str> = meta.split_whitespace().collect();
        if cols.len() < 4 {
            continue;
        }
        let (mode, _typ, sha, size) = (cols[0], cols[1], cols[2], cols[3]);
        rows.push(TreeRaw {
            path: path.to_string(),
            mode: mode.to_string(),
            entry_type: entry_type(mode).to_string(),
            blob_sha: sha.to_string(),
            size: size.parse().ok(), // '-' for submodules → None
        });
    }
    Ok(rows)
}
