//!  — submodules: the `.gitmodules` declarations joined to the gitlink
//! pins in the HEAD tree.
//!
//! A submodule has two independent facts in git, and neither table so far
//! carried the important one — the URL:
//!   - `.gitmodules` (a tracked file at HEAD) DECLARES `path`, `url`, `branch`
//!     under `[submodule "<name>"]`. This is where the submodule points at *in
//!     the world* — not recoverable from the object store.
//!   - the HEAD tree PINS each submodule path to a specific commit via a
//!     `160000` gitlink entry (`blob_sha` = the pinned commit).
//!
//! We join them by path, so a consumer sees `url` + `pinned_sha` together, and we
//! keep the two presence flags because they can disagree (a `.gitmodules` entry
//! whose gitlink was removed, or a bare gitlink with no `.gitmodules` stanza).
//! Parsing is delegated to git's own config parser reading the blob directly
//! (`git config --blob HEAD:.gitmodules`), so odd-but-valid config never trips a
//! hand-rolled INI reader. HEAD-derived — recomputed every run, like refs/tree.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

use crate::model::SubmoduleRow;

/// One declared submodule from `.gitmodules` (before the gitlink join). Keyed by
/// section NAME while parsing; re-keyed to its declared `path` for the join.
#[derive(Default)]
struct Declared {
    name: String,
    path: Option<String>,
    url: Option<String>,
    branch: Option<String>,
}

/// Read `.gitmodules` at HEAD via git's config parser. Returns declared-path ->
/// declaration. Empty (not an error) when the repo has no `.gitmodules`.
fn declared(repo_path: &str) -> BTreeMap<String, Declared> {
    // `git config --blob HEAD:.gitmodules -l -z`: NUL-separated `key\nvalue`
    // records. Fails cleanly (non-zero) when the blob doesn't exist.
    let out = Command::new("git")
        .args(["-C", repo_path, "config", "--blob", "HEAD:.gitmodules", "-l", "-z"])
        .output();
    let stdout = match out {
        Ok(o) if o.status.success() => o.stdout,
        _ => return BTreeMap::new(),
    };
    let text = String::from_utf8_lossy(&stdout);

    // Accumulate by section name first (fields arrive in any order).
    let mut by_name: BTreeMap<String, Declared> = BTreeMap::new();
    for record in text.split('\0').filter(|s| !s.is_empty()) {
        // record = "submodule.<name>.<field>\n<value>"; <name> may itself contain
        // dots, so peel the fixed prefix and split off the LAST dot for the field.
        let (key, value) = record.split_once('\n').unwrap_or((record, ""));
        let Some(rest) = key.strip_prefix("submodule.") else { continue };
        let Some(dot) = rest.rfind('.') else { continue };
        let (name, field) = (&rest[..dot], &rest[dot + 1..]);
        let entry = by_name.entry(name.to_string()).or_default();
        entry.name = name.to_string();
        match field {
            "path" => entry.path = Some(value.to_string()),
            "url" => entry.url = Some(value.to_string()),
            "branch" => entry.branch = Some(value.to_string()),
            _ => {}
        }
    }

    // Re-key by declared path. A stanza with no `path` is malformed — git ignores
    // it, so we do too.
    let mut by_path: BTreeMap<String, Declared> = BTreeMap::new();
    for (_name, decl) in by_name {
        if let Some(path) = decl.path.clone() {
            by_path.insert(path, decl);
        }
    }
    by_path
}

/// Gitlink pins from the HEAD tree: path -> pinned commit sha (mode 160000).
fn gitlinks(repo_path: &str) -> Result<BTreeMap<String, String>> {
    let out = Command::new("git")
        .args(["-C", repo_path, "ls-tree", "-r", "-z", "HEAD"])
        .output()
        .context("git ls-tree -r HEAD")?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut m = BTreeMap::new();
    for entry in text.split('\0').filter(|s| !s.is_empty()) {
        let (meta, path) = match entry.split_once('\t') {
            Some(x) => x,
            None => continue,
        };
        let cols: Vec<&str> = meta.split_whitespace().collect();
        if cols.len() >= 3 && cols[0] == "160000" {
            m.insert(path.to_string(), cols[2].to_string());
        }
    }
    Ok(m)
}

/// Compute the submodule table for HEAD. Empty when the repo has no submodules.
pub fn compute(repo_path: &Path) -> Result<Vec<SubmoduleRow>> {
    let root = repo_path.to_string_lossy().to_string();
    let decl = declared(&root);
    let links = gitlinks(&root)?;

    // Union of paths from both sources, so a disagreement is visible, not dropped.
    let mut paths: BTreeMap<String, ()> = BTreeMap::new();
    for p in decl.keys() { paths.insert(p.clone(), ()); }
    for p in links.keys() { paths.insert(p.clone(), ()); }

    let mut rows = Vec::new();
    for path in paths.into_keys() {
        let d = decl.get(&path);
        let pinned = links.get(&path).cloned();
        rows.push(SubmoduleRow {
            path: path.clone(),
            name: d.map(|d| d.name.clone()),
            url: d.and_then(|d| d.url.clone()),
            branch: d.and_then(|d| d.branch.clone()),
            pinned_sha: pinned.clone(),
            in_gitmodules: d.is_some(),
            in_tree: pinned.is_some(),
        });
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A throwaway repo with one real submodule gitlink + a matching .gitmodules,
    // exercised end to end through git's own plumbing.
    #[test]
    fn parses_gitmodules_and_joins_gitlink() {
        let dir = std::env::temp_dir().join(format!("gi-sub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sh = |args: &[&str]| {
            let ok = Command::new("git").args(args).current_dir(&dir).output().unwrap();
            assert!(ok.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&ok.stderr));
        };
        sh(&["init", "-q"]);
        sh(&["config", "user.email", "t@t"]);
        sh(&["config", "user.name", "t"]);
        // A .gitmodules declaring a submodule at path "vendor/lib".
        std::fs::write(
            dir.join(".gitmodules"),
            "[submodule \"vendor/lib\"]\n\tpath = vendor/lib\n\turl = https://example.com/lib.git\n\tbranch = main\n",
        ).unwrap();
        // A gitlink at the same path, pinned to an arbitrary 40-hex.
        let pin = "0123456789012345678901234567890123456789";
        sh(&["update-index", "--add", "--cacheinfo", &format!("160000,{pin},vendor/lib")]);
        sh(&["add", ".gitmodules"]);
        sh(&["commit", "-q", "-m", "add submodule"]);

        let rows = compute(&dir).unwrap();
        assert_eq!(rows.len(), 1, "one submodule");
        let r = &rows[0];
        assert_eq!(r.path, "vendor/lib");
        assert_eq!(r.name.as_deref(), Some("vendor/lib"));
        assert_eq!(r.url.as_deref(), Some("https://example.com/lib.git"));
        assert_eq!(r.branch.as_deref(), Some("main"));
        assert_eq!(r.pinned_sha.as_deref(), Some(pin));
        assert!(r.in_gitmodules && r.in_tree);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_when_no_submodules() {
        let dir = std::env::temp_dir().join(format!("gi-nosub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sh = |args: &[&str]| {
            Command::new("git").args(args).current_dir(&dir).output().unwrap();
        };
        sh(&["init", "-q"]);
        sh(&["config", "user.email", "t@t"]);
        sh(&["config", "user.name", "t"]);
        std::fs::write(dir.join("a.txt"), "hi\n").unwrap();
        sh(&["add", "a.txt"]);
        sh(&["commit", "-q", "-m", "x"]);
        assert!(compute(&dir).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
