//! gitindex — the EOS git-index engine ().
//!
//! Reads a repo's object store directly and materialises the frozen L1 core
//! schema as Parquet — checked against the same git oracles via `eng/bench`.
//!
//!   gitindex index <repo-path> --out ./index/         # incremental if possible
//!   gitindex index <repo-path> --out ./index/ --full  # force a full reindex
//!
//! With an existing index whose recorded HEAD is an ancestor of the current
//! HEAD, only the delta is processed (); otherwise a full index runs.

mod blame;
mod chunk;
mod deps;
mod diff;
mod incremental;
mod blobs;
mod generated;
mod markers;
mod trees;
mod ingest;
mod model;
mod read;
mod refs;
mod reftable;
mod rename;
mod secrets;
mod snapshots;
mod submodule;
mod testfiles;
mod symbols;
mod tree;
mod writer;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Parser)]
#[command(
    name = "gitindex",
    version,
    about = "Turn a git repo into queryable Parquet (SQL), verified against git.",
    long_about = "gitindex reads a repository's object store and materialises a fresh, complete, \
                  queryable SQL database as Parquet — commits, diffs, renames, blame, ancestry. \
                  Query it with DuckDB: duckdb -c \"SELECT * FROM './index/commits.parquet'\"."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Index a repository into Parquet (L1 core schema).
    Index {
        /// Path to the git repository.
        repo: PathBuf,
        /// Output directory for the Parquet tables.
        #[arg(long, default_value = "./index")]
        out: PathBuf,
        /// Force a full reindex even if an incremental update is possible.
        #[arg(long)]
        full: bool,
        /// Which refs to index: head (default branch), active, all, or auto
        /// (). `auto` (the default) follows the tier: `high` indexes the
        /// default + active branches (abandoned ones deferred), basic/mid only head.
        #[arg(long, default_value = "auto")]
        refs: String,
        /// Shared content-addressed blob-fact cache dir — the cross-repo dedup
        /// cache / "moat" (). Reports dedup_hit_rate.
        #[arg(long)]
        cache: Option<PathBuf>,
    },
}

/// Parquet layout version, recorded in manifest.json and CHECKED before an
/// incremental update (read.rs reads columns by position, so a layout change is
/// only safe with a matching version — otherwise a full reindex is forced).
/// BUMP THIS whenever a table read by `read_old` gains/loses/reorders a column
/// (commits, commit_files, hunks, commit_messages, commit_trailers, blame, …).
/// v2: E1–E5 added committer/offset + commit_messages/trailers columns, hunks,
/// commit_files src_mode/dst_mode, and the refs/tree_entries/submodules tables.
const SCHEMA_VERSION: u32 = 2;

fn head_sha(repo: &Path) -> Result<String> {
    let out = Command::new("git")
        .args(["-C", &repo.to_string_lossy(), "rev-parse", "HEAD"])
        .output()
        .context("git rev-parse HEAD")?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Cheap size estimate (commit count + HEAD file count) for the level hint.
fn repo_estimate(repo: &Path) -> (u64, u64) {
    let root = repo.to_string_lossy().to_string();
    let commits = Command::new("git")
        .args(["-C", &root, "rev-list", "--count", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
        .unwrap_or(0);
    let files = Command::new("git")
        .args(["-C", &root, "ls-tree", "-r", "--name-only", "HEAD"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().count() as u64)
        .unwrap_or(0);
    (commits, files)
}

/// Advisory level hint (C2): never changes the level, just nudges when the repo
/// size makes the effective level a poor fit.
fn level_hint(repo: &Path, level: ingest::Level) {
    let (commits, files) = repo_estimate(repo);
    if commits == 0 {
        return;
    }
    let suggested = ingest::Level::suggest(commits, files);
    if !ingest::Level::env_set() && suggested < level {
        eprintln!(
            "  hint: large repo ({commits} commits, {files} files) — running '{}' (default). \
             Set EOS_INDEX_LEVEL=basic for a faster, lighter index if you only need L1.",
            level.tag()
        );
    }
    if level == ingest::Level::High && suggested < ingest::Level::High {
        eprintln!(
            "  note: 'high' re-blames every tag and dedups all history — on a repo this size \
             ({commits} commits) that is slow and memory-heavy. Prefer 'mid', or set \
             EOS_SNAPSHOTS/EOS_CHUNK selectively."
        );
    }
}

/// What a prior index recorded in manifest.json.
struct Manifest {
    head_sha: String,
    /// The schema version the prior index was WRITTEN with. `None` for a legacy
    /// manifest that predates the field — treated as incompatible, since its
    /// column layout can't be assumed to match the current reader.
    schema_version: Option<u32>,
}

fn parse_json_str(text: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":\"");
    let start = text.find(&pat)? + pat.len();
    let rest = &text[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}
fn parse_json_u32(text: &str, key: &str) -> Option<u32> {
    let pat = format!("\"{key}\":");
    let start = text.find(&pat)? + pat.len();
    let rest = text[start..].trim_start();
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// The prior index's manifest, if one exists.
fn read_manifest(out: &Path) -> Option<Manifest> {
    let text = std::fs::read_to_string(out.join("manifest.json")).ok()?;
    Some(Manifest {
        head_sha: parse_json_str(&text, "head_sha")?,
        schema_version: parse_json_u32(&text, "schema_version"),
    })
}

fn write_manifest(out: &Path, head: &str) -> Result<()> {
    std::fs::write(
        out.join("manifest.json"),
        format!("{{\"schema_version\":{SCHEMA_VERSION},\"head_sha\":\"{head}\"}}\n"),
    )?;
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Index { repo, out, full, refs, cache } => {
            std::fs::create_dir_all(&out)?;
            let new_head = head_sha(&repo)?;
            let prior = read_manifest(&out);
            // Incremental is only safe when the prior index was written with the
            // SAME schema version: read_old (read.rs) reads Parquet by column
            // POSITION, so any layout change across binary versions would silently
            // misread columns. On a version mismatch — or a legacy manifest with no
            // version — fall back to a full reindex instead of corrupting the data.
            let schema_ok = matches!(prior.as_ref().and_then(|m| m.schema_version), Some(v) if v == SCHEMA_VERSION);
            let old_head = prior.as_ref().map(|m| m.head_sha.clone());
            if old_head.is_some() && !schema_ok && !full {
                let was = prior.as_ref().and_then(|m| m.schema_version);
                eprintln!(
                    "  note: prior index schema_version {} != {SCHEMA_VERSION} — forcing full reindex (incremental would misread columns)",
                    was.map(|v| v.to_string()).unwrap_or_else(|| "none".into()),
                );
            }
            // `auto` (the default) ties branch coverage to the tier, like table
            // coverage: `high` = the whole picture (default + active branches, the
            // abandoned ones intelligently deferred), basic/mid = the hot path (head).
            let refs_resolved = if refs == "auto" {
                if ingest::Level::from_env() == ingest::Level::High { "active" } else { "head" }
            } else {
                refs.as_str()
            };
            let ref_mode = refs::RefMode::parse(refs_resolved)
                .ok_or_else(|| anyhow::anyhow!("--refs must be head|active|all|auto"))?;
            level_hint(&repo, ingest::Level::from_env());
            let t0 = std::time::Instant::now();

            let (ing, mode) = if ref_mode != refs::RefMode::Head {
                // Multi-ref index (): full over the selected tips.
                let sel = refs::select(&repo, ref_mode)?;
                eprintln!("  refs: indexing {} ({} tips)", sel.selected.join(", "), sel.tips.len());
                if !sel.deferred.is_empty() {
                    eprintln!("  refs deferred (abandoned, cheap cache-fill later): {}", sel.deferred.join(", "));
                }
                (ingest::ingest_tips(&repo, sel.tips, cache.clone())?, "full/refs")
            } else {
                match old_head {
                    Some(old) if !full && schema_ok && incremental::is_ancestor(&repo, &old, &new_head) => {
                        (incremental::ingest_incremental(&repo, &out, &old, &new_head, cache.clone())?, "incremental")
                    }
                    _ => (ingest::ingest(&repo, cache.clone())?, "full"),
                }
            };

            // Refs (branches/tags/HEAD) — cheap, always current, recomputed every
            // run regardless of full/incremental.
            let mut ing = ing;
            ing.refs = reftable::compute(&repo)?;
            ing.notes = reftable::compute_notes(&repo)?;
            // Submodules: .gitmodules declarations joined to HEAD gitlink pins.
            ing.submodules = submodule::compute(&repo)?;
            // Dependencies: parse HEAD manifests (package.json, Cargo.toml, …).
            ing.dependencies = deps::compute(&repo)?;
            // HEAD tree with modes, stitched to file_ids (every HEAD path is in
            // `files` via blame/changes; any that isn't is logged, not silently
            // dropped — the oracle would catch a count mismatch anyway).
            let path2id: std::collections::HashMap<String, i64> =
                ing.files.iter().map(|f| (f.path.clone(), f.file_id)).collect();
            let mut skipped = 0u64;
            ing.tree_entries = tree::head_entries(&repo)?
                .into_iter()
                .filter_map(|t| match path2id.get(&t.path) {
                    Some(&file_id) => Some(model::TreeEntryRow {
                        file_id,
                        path: t.path,
                        mode: t.mode,
                        entry_type: t.entry_type,
                        blob_sha: t.blob_sha,
                        size: t.size,
                    }),
                    None => {
                        skipped += 1;
                        None
                    }
                })
                .collect();
            if skipped > 0 {
                eprintln!("  tree: {skipped} HEAD entries had no file_id (skipped)");
            }
            // Test files — path-based, so derive straight from the HEAD tree
            // entries (always on, no content).
            ing.test_files = ing
                .tree_entries
                .iter()
                .filter_map(|t| testfiles::detect(&t.path).map(|(lang, signal)| model::TestFileRow {
                    file_id: t.file_id,
                    lang: lang.to_string(),
                    signal: signal.to_string(),
                }))
                .collect();
            // Generated / vendored files — path-based, so derive straight from the
            // HEAD tree entries (always on, no content).
            ing.generated_files = ing
                .tree_entries
                .iter()
                .filter_map(|t| generated::classify(&t.path).map(|(category, reason)| model::GeneratedFileRow {
                    file_id: t.file_id,
                    category: category.to_string(),
                    reason: reason.to_string(),
                }))
                .collect();
            // Test→source coverage (name-based), stitched to file_ids.
            let head_paths: Vec<String> = ing.tree_entries.iter().map(|t| t.path.clone()).collect();
            ing.test_coverage = testfiles::coverage(&head_paths)
                .into_iter()
                .filter_map(|(tp, sp, method)| match (path2id.get(&tp), path2id.get(&sp)) {
                    (Some(&t), Some(&s)) => Some(model::TestCoverageRow { test_file_id: t, source_file_id: s, method: method.to_string() }),
                    _ => None,
                })
                .collect();
            // Code markers (TODO/FIXME/…) — a content scan, so mid+ tier. Stitched
            // to file_ids like tree_entries; a HEAD path always has one.
            if ingest::Level::from_env() >= ingest::Level::Mid {
                ing.code_markers = markers::compute(&repo)?
                    .into_iter()
                    .filter_map(|m| path2id.get(&m.path).map(|&file_id| model::CodeMarkerRow {
                        file_id,
                        line: m.line,
                        marker: m.marker,
                        text: m.text,
                    }))
                    .collect();
                // Generated by CONTENT marker — a codegen header in files the path
                // rules miss. Path detection already ran (always on) and wins, so
                // only add files not already flagged. mid+ (reads blobs).
                let already: std::collections::HashSet<i64> =
                    ing.generated_files.iter().map(|g| g.file_id).collect();
                for path in generated::compute_content(&repo)? {
                    if let Some(&file_id) = path2id.get(&path) {
                        if !already.contains(&file_id) {
                            ing.generated_files.push(model::GeneratedFileRow {
                                file_id,
                                category: "generated".to_string(),
                                reason: "content-marker".to_string(),
                            });
                        }
                    }
                }
                ing.generated_files.sort_by_key(|g| g.file_id);
                // Secret detection — same HEAD content scan, mid+ tier.
                ing.secret_findings = secrets::compute(&repo)?
                    .into_iter()
                    .filter_map(|s| path2id.get(&s.path).map(|&file_id| model::SecretFindingRow {
                        file_id,
                        line: s.line,
                        rule: s.rule,
                        preview: s.preview,
                    }))
                    .collect();
                // Content-addressed per-blob facts (the shared cache's lexical layer): one
                // row per DISTINCT HEAD blob, keyed by content. Shas come from the
                // HEAD tree entries; compute reads each unique blob once.
                let head_shas: Vec<String> = ing.tree_entries.iter().map(|t| t.blob_sha.clone()).collect();
                ing.blob_facts = blobs::compute(&repo, &head_shas)?;
            }
            writer::write_all(&out, &ing)?;
            write_manifest(&out, &new_head)?;
            // Self-describing index card (read back from the Parquet footers).
            writer::write_index_card(&out, SCHEMA_VERSION, ingest::Level::from_env().tag(), &new_head)?;
            eprintln!(
                "{mode} index [{}]: {} commits, {} files, {} file-changes, {} blame lines, {} symbols, {} refs in {:.2}s -> {}",
                ingest::Level::from_env().tag(),
                ing.commits.len(),
                ing.files.len(),
                ing.commit_files.len(),
                ing.blame.len(),
                ing.symbols.len(),
                ing.symbol_refs.len(),
                t0.elapsed().as_secs_f64(),
                out.display(),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_parse_current_and_legacy() {
        let cur = format!("{{\"schema_version\":{SCHEMA_VERSION},\"head_sha\":\"abc123\"}}\n");
        assert_eq!(parse_json_str(&cur, "head_sha").as_deref(), Some("abc123"));
        assert_eq!(parse_json_u32(&cur, "schema_version"), Some(SCHEMA_VERSION));

        // Legacy manifest (predates the field): head parses, version is None →
        // treated as incompatible so incremental falls back to a full reindex.
        let legacy = "{\"head_sha\":\"deadbeef\"}\n";
        assert_eq!(parse_json_str(legacy, "head_sha").as_deref(), Some("deadbeef"));
        assert_eq!(parse_json_u32(legacy, "schema_version"), None);
    }

    #[test]
    fn manifest_version_ordering_gates_incremental() {
        // A different (older) version must not compare equal to the current one.
        let old = "{\"schema_version\":1,\"head_sha\":\"x\"}\n";
        let v = parse_json_u32(old, "schema_version");
        assert_eq!(v, Some(1));
        assert!(!matches!(v, Some(n) if n == SCHEMA_VERSION), "v1 must not match current schema");
    }
}
