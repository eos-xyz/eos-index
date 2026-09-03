//!  — git refs (branches, tags, remote-tracking, symbolic HEAD) as a
//! table. One `git for-each-ref` pass (plus HEAD, which for-each-ref omits). Cheap
//! and always current, so it's recomputed on every index (full and incremental).

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

use crate::model::{NoteRow, RefRow};

const US: char = '\u{1f}'; // unit separator between fields (can't appear in refnames)

fn strip_angle(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    Some(s.trim_start_matches('<').trim_end_matches('>').to_string())
}

fn opt(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

fn kind_of(name: &str, objtype: &str, is_symbolic: bool) -> String {
    if is_symbolic {
        "symbolic"
    } else if name.starts_with("refs/heads/") {
        "branch"
    } else if name.starts_with("refs/remotes/") {
        "remote-branch"
    } else if name.starts_with("refs/tags/") {
        if objtype == "tag" { "tag-annotated" } else { "tag-lightweight" }
    } else {
        "other"
    }
    .to_string()
}

/// Peeled commit: the commit the ref resolves to. For an annotated tag that's the
/// dereferenced (`*objectname`) commit; for anything already a commit it's the
/// object itself; for a tag on a blob/tree it's None.
fn peeled(objtype: &str, objname: &str, star_type: &str, star_name: &str) -> Option<String> {
    if !star_name.is_empty() && star_type == "commit" {
        Some(star_name.to_string())
    } else if objtype == "commit" {
        Some(objname.to_string())
    } else {
        None
    }
}

pub fn compute(repo_path: &Path) -> Result<Vec<RefRow>> {
    let root = repo_path.to_string_lossy().to_string();
    // refname, objtype, objname, *objname, *objtype, symref, tagger, taggeremail, taggerdate:unix, tag subject
    let fmt = "%(refname)\u{1f}%(objecttype)\u{1f}%(objectname)\u{1f}%(*objectname)\u{1f}%(*objecttype)\u{1f}%(symref)\u{1f}%(taggername)\u{1f}%(taggeremail)\u{1f}%(taggerdate:unix)\u{1f}%(contents:subject)";
    let out = Command::new("git")
        .args(["-C", &root, "for-each-ref", &format!("--format={fmt}")])
        .output()
        .context("git for-each-ref")?;
    let text = String::from_utf8_lossy(&out.stdout);

    let mut rows = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split(US).collect();
        if f.len() < 10 {
            continue;
        }
        let (name, objtype, objname, star_name, star_type, symref) = (f[0], f[1], f[2], f[3], f[4], f[5]);
        let is_symbolic = !symref.trim().is_empty();
        // Annotated tag bodies can contain newlines (they'd break this line-based
        // parse), so fetch each one on its own — one ref, its whole body is the
        // output. Lightweight tags (objtype != "tag") have no message.
        let tag_body = if objtype == "tag" { fetch_tag_body(&root, name) } else { None };
        rows.push(RefRow {
            name: name.to_string(),
            kind: kind_of(name, objtype, is_symbolic),
            object_sha: objname.to_string(),
            peeled_commit_sha: peeled(objtype, objname, star_type, star_name),
            is_symbolic,
            tagger_name: opt(f[6]),
            tagger_email: strip_angle(f[7]),
            tagged_at_epoch: opt(f[8]).and_then(|s| s.parse().ok()),
            tag_subject: opt(f[9]),
            tag_body,
        });
    }

    // HEAD is not under refs/, so for-each-ref skips it — add it if it's symbolic.
    if let Ok(o) = Command::new("git").args(["-C", &root, "symbolic-ref", "-q", "HEAD"]).output() {
        if o.status.success() {
            let target = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let head_sha = Command::new("git")
                .args(["-C", &root, "rev-parse", "HEAD"])
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            if !target.is_empty() {
                rows.push(RefRow {
                    name: "HEAD".to_string(),
                    kind: "symbolic".to_string(),
                    object_sha: head_sha.clone(),
                    peeled_commit_sha: (!head_sha.is_empty()).then_some(head_sha),
                    is_symbolic: true,
                    tagger_name: None,
                    tagger_email: None,
                    tagged_at_epoch: None,
                    tag_subject: Some(target), // the ref HEAD points at
                    tag_body: None,
                });
            }
        }
    }

    rows.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(rows)
}

/// The body of one annotated tag (its message below the subject), or None if empty.
fn fetch_tag_body(root: &str, name: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["-C", root, "for-each-ref", "--format=%(contents:body)", name])
        .output()
        .ok()?;
    let body = String::from_utf8_lossy(&out.stdout);
    let t = body.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// git notes (): every note across every `refs/notes/*`, with its text.
/// Empty when the repo uses no notes (the common case) — zero extra work then.
pub fn compute_notes(repo_path: &Path) -> Result<Vec<NoteRow>> {
    let root = repo_path.to_string_lossy().to_string();
    // The notes refs (each is an independent notes namespace).
    let refs_out = Command::new("git")
        .args(["-C", &root, "for-each-ref", "--format=%(refname)", "refs/notes"])
        .output()
        .context("git for-each-ref refs/notes")?;
    let mut out = Vec::new();
    for notes_ref in String::from_utf8_lossy(&refs_out.stdout).lines().filter(|s| !s.is_empty()) {
        // `git notes --ref=X list` → "<note-blob-sha> <annotated-object-sha>" per line.
        let list = Command::new("git")
            .args(["-C", &root, "notes", &format!("--ref={notes_ref}"), "list"])
            .output();
        let Ok(list) = list else { continue };
        for line in String::from_utf8_lossy(&list.stdout).lines() {
            let mut it = line.split_whitespace();
            let (Some(note_sha), Some(target_sha)) = (it.next(), it.next()) else { continue };
            let blob = Command::new("git")
                .args(["-C", &root, "cat-file", "blob", note_sha])
                .output();
            let body = blob.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
            out.push(NoteRow {
                notes_ref: notes_ref.to_string(),
                target_sha: target_sha.to_string(),
                note_sha: note_sha.to_string(),
                body,
            });
        }
    }
    Ok(out)
}
