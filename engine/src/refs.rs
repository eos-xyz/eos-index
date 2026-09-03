//!  — branch-topology prioritisation. A ref-selection policy over the
//! ingest work-set: index the default branch and *active* branches first, and
//! defer *abandoned* ones (a cost lever — indexing every ref of a big repo is
//! wasteful). With content-addressing, a deferred branch is a cheap cache-fill
//! later, not a reindex. Nothing is dropped silently: deferred refs are logged.

use std::process::Command;

use anyhow::Result;
use gix::ObjectId;

#[derive(Clone, Copy, PartialEq)]
pub enum RefMode {
    Head,   // default branch only (HEAD) — the hot path
    Active, // default + branches active within the window
    All,    // every branch
}

impl RefMode {
    pub fn parse(s: &str) -> Option<RefMode> {
        match s {
            "head" => Some(RefMode::Head),
            "active" => Some(RefMode::Active),
            "all" => Some(RefMode::All),
            _ => None,
        }
    }
}

/// A branch tip and when it was last committed to.
struct Tip {
    oid: ObjectId,
    committed_at: i64,
    name: String,
}

const ACTIVE_WINDOW_SECS: i64 = 90 * 24 * 3600; // 90 days behind the newest tip

pub struct Selection {
    pub tips: Vec<ObjectId>,     // tips to walk (deduped; default first)
    pub selected: Vec<String>,   // ref names indexed
    pub deferred: Vec<String>,   // ref names deferred (logged, not dropped)
}

fn git(repo: &str, args: &[&str]) -> Result<String> {
    let out = Command::new("git").arg("-C").arg(repo).args(args).output()?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Resolve the tips to index under `mode`, prioritising default + active refs.
pub fn select(repo_path: &std::path::Path, mode: RefMode) -> Result<Selection> {
    let repo = repo_path.to_string_lossy().to_string();
    let head_oid = ObjectId::from_hex(git(&repo, &["rev-parse", "HEAD"])?.trim().as_bytes())?;

    if mode == RefMode::Head {
        return Ok(Selection { tips: vec![head_oid], selected: vec!["HEAD".into()], deferred: vec![] });
    }

    // All branch tips (local + remote-tracking), with commit dates.
    let listing = git(
        &repo,
        &["for-each-ref", "--format=%(objectname)\t%(committerdate:unix)\t%(refname:short)", "refs/heads", "refs/remotes"],
    )?;
    let mut tips: Vec<Tip> = Vec::new();
    for line in listing.lines() {
        let mut it = line.split('\t');
        let (Some(oid), Some(date), Some(name)) = (it.next(), it.next(), it.next()) else { continue };
        if name.ends_with("/HEAD") {
            continue; // origin/HEAD symref
        }
        if let (Ok(oid), Ok(date)) = (ObjectId::from_hex(oid.as_bytes()), date.parse::<i64>()) {
            tips.push(Tip { oid, committed_at: date, name: name.to_string() });
        }
    }

    let newest = tips.iter().map(|t| t.committed_at).max().unwrap_or(0);
    // Default branch (HEAD's tip) always leads and is always selected.
    tips.sort_by(|a, b| b.committed_at.cmp(&a.committed_at)); // most recent first

    let mut seen = std::collections::HashSet::new();
    let mut ordered = vec![head_oid];
    seen.insert(head_oid);
    let mut selected = vec!["HEAD".to_string()];
    let mut deferred = Vec::new();
    for t in tips {
        // Active = touched within the window (or `all`). No cap: `high` indexes every
        // active branch even on a repo with hundreds of them — slower, but complete.
        // Only genuinely-abandoned branches (stale beyond the window) are deferred.
        let active = mode == RefMode::All || newest - t.committed_at <= ACTIVE_WINDOW_SECS;
        if active {
            if seen.insert(t.oid) {
                ordered.push(t.oid);
            }
            selected.push(t.name);
        } else {
            deferred.push(t.name);
        }
    }
    Ok(Selection { tips: ordered, selected, deferred })
}
