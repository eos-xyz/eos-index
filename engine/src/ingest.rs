//! /3.3 — walk the commit graph and tree-diff each non-merge commit.
//! Split into `walk` (gather path-keyed raw parts over a commit range) and
//! `assemble` (turn raw parts + blame into the id-keyed L1 tables), so both the
//! full index and the incremental update () share the assembly.

use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use crate::blame::{compute_blame, compute_blame_native, BlameRow};
use crate::diff::{diff_commit, Change, Stats};
use crate::model::{AreaOwnershipRow, AuthorRow, BlameLineRow, CollaborationRow, CommitClassRow, CommitFileRow, CommitRow, CommitStatRow, CouplingRow, FileOwnershipRow, FileRow, HunkRow, IdentityRow, Ingested, InsightRow, MergeChangeRow, MessageRow, ModuleDepRow, ParentRow, SnapshotBlameRow, SnapshotOwnershipRow, SymbolEdgeRow, SymbolRefRow, SymbolRow, TrailerRow};

/// Conventional-Commits types (the closed set the spec + common practice use).
const CONVENTIONAL_TYPES: &[&str] =
    &["feat", "fix", "docs", "style", "refactor", "perf", "test", "build", "ci", "chore", "revert"];

/// Classify a commit from its subject → (kind, scope, is_conventional, is_breaking).
/// Conventional `type(scope)!: …` wins; else a small keyword heuristic; `merge` for
/// merges. Deterministic and pure (the bench oracle recomputes the same rule).
fn classify_commit(subject: &str, is_merge: bool) -> (String, String, bool, bool) {
    if is_merge {
        return ("merge".to_string(), String::new(), false, false);
    }
    // Conventional: header before the first ':' is `type` + optional `(scope)` +
    // optional `!`. The type must be one of the known set to count as conventional.
    if let Some(colon) = subject.find(':') {
        let header = subject[..colon].trim();
        let (mut head, breaking) = match header.strip_suffix('!') {
            Some(h) => (h.trim(), true),
            None => (header, false),
        };
        let mut scope = String::new();
        if let (Some(open), true) = (head.find('('), head.ends_with(')')) {
            scope = head[open + 1..head.len() - 1].trim().to_string();
            head = head[..open].trim();
        }
        let ty = head.to_ascii_lowercase();
        if CONVENTIONAL_TYPES.contains(&ty.as_str()) {
            return (ty, scope, true, breaking);
        }
    }
    // Heuristic fallback on the leading word (non-conventional subjects).
    let lower = subject.trim_start().to_ascii_lowercase();
    let first = lower.split(|c: char| !c.is_ascii_alphabetic()).next().unwrap_or("");
    let kind = match first {
        "fix" | "fixed" | "fixes" | "bug" | "bugfix" | "hotfix" => "fix",
        "add" | "added" | "feat" | "feature" | "implement" | "introduce" | "new" => "feat",
        "doc" | "docs" | "documentation" | "readme" => "docs",
        "test" | "tests" | "testing" => "test",
        "refactor" | "refactored" | "cleanup" | "rename" | "move" | "reorganize" => "refactor",
        "perf" | "optimize" | "optimise" | "speed" => "perf",
        "revert" => "revert",
        "bump" | "chore" | "release" | "deps" | "dependency" | "dependencies" => "chore",
        "merge" => "merge",
        _ => "other",
    };
    (kind.to_string(), String::new(), false, false)
}

// Analytics thresholds (+), matching the dbt marts' defaults so the
// engine tables and the SQL marts mean the same thing.
/// A commit touching more files than this is a bulk move/reformat — excluded from
/// coupling/collaboration so it doesn't couple everything to everything.
const BULK_COMMIT_MAX_FILES: usize = 50;
/// A file pair must co-change at least this many times to be "coupled".
const COUPLING_MIN_COCHANGES: i64 = 3;
/// A file touched by more than this many people is a "hub" (lockfile, global) —
/// excluded from collaboration so it doesn't couple everyone to everyone.
const COLLAB_MAX_EDITORS: usize = 25;

// Insights () — thresholds for the briefing rules. Shared with the
// bench oracle so the engine rows and the definitional SQL agree exactly.
const BUS_MIN_LINES: i64 = 50; // ignore tiny files
const BUS_CRIT_SHARE: f64 = 0.90; // one person owns ≥ 90% of a file → critical
const AREA_MIN_LINES: i64 = 200; // ignore tiny areas
const AREA_KEY_SHARE: f64 = 0.80; // one person owns ≥ 80% of a module
const HOTSPOT_MIN_CHANGES: i64 = 25; // a file changed this many times is a hotspot
const HIDDEN_COUPLING_MIN: i64 = 8; // cross-area pairs co-changing this often
const HUB_MIN_DEPENDENTS: i64 = 15; // files depended on by this many others are hubs
const FRAGILE_HUB_MIN_CHANGES: i64 = 15; // a hub that also churns this often is fragile
/// A briefing is headlines, not an exhaustive dump: each kind emits at most this
/// many rows, the top ones by that kind's salience (so a solo repo doesn't drown
/// the reader in "everything is single-owned").
const INSIGHTS_TOP_N: usize = 20;

/// Is this person a bot (dependabot/renovate/CI…)? Matches the dbt marts'
/// `is_bot` rule so the engine table and the SQL marts agree.
fn is_bot_identity(name: &str, email: &str) -> bool {
    let (n, e) = (name.to_ascii_lowercase(), email.to_ascii_lowercase());
    n.contains("[bot]")
        || e.contains("[bot]")
        || e.contains("dependabot")
        || e.contains("renovate")
        || e.contains("github-actions")
        || matches!(n.as_str(), "dependabot" | "renovate" | "github-actions" | "github actions")
}

/// How much to precompute (). Trades index time + memory for access
/// speed — the customer picks the tier that fits their repo and machine.
/// `EOS_INDEX_LEVEL` selects it; default `mid`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Level {
    /// L1 fundamentals only: commits, parents, files, changes, authors,
    /// identities, blame. Fast, low memory.
    Basic,
    /// + materialized ownership and L3 symbols/references. The balanced default.
    Mid,
    /// + the symbol call-graph (GRAIL), and — unless their env overrides —
    /// historical blame snapshots (tags) and content dedup (FastCDC). Max access,
    /// most time and memory.
    High,
}

impl Level {
    pub fn from_env() -> Level {
        match std::env::var("EOS_INDEX_LEVEL").ok().as_deref().map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("basic") => Level::Basic,
            Some("high") => Level::High,
            _ => Level::Mid,
        }
    }
    pub fn tag(self) -> &'static str {
        match self {
            Level::Basic => "basic",
            Level::Mid => "mid",
            Level::High => "high",
        }
    }
    /// Whether the user chose a level explicitly (vs. falling back to the default).
    pub fn env_set() -> bool {
        std::env::var("EOS_INDEX_LEVEL").ok().is_some_and(|v| !v.trim().is_empty())
    }
    /// A level suggestion from repo size — ADVISORY ONLY (we never change the level
    /// silently, per C2). A very large repo is expensive at `mid`/`high`; a small
    /// one can afford `high`. `mid` is the balanced middle.
    pub fn suggest(commits: u64, files: u64) -> Level {
        if commits > 200_000 || files > 40_000 {
            Level::Basic
        } else if commits < 2_000 && files < 3_000 {
            Level::High
        } else {
            Level::Mid
        }
    }
}
use crate::symbols::{compute_l3, SymbolRaw, SymbolRefRaw};

/// Path-keyed intermediate: what `walk` produces and `assemble` consumes.
pub struct RawParts {
    pub commits: Vec<CommitRow>,
    pub messages: Vec<MessageRow>,
    pub trailers: Vec<TrailerRow>,
    pub parents: Vec<ParentRow>,
    pub authors: Vec<AuthorRow>, // may contain duplicates; assemble dedups
    pub changes: Vec<(String, Change)>, // (commit_sha, change with path strings)
    pub merge_changes: Vec<(String, Change)>, // merge commits' first-parent changes
}

/// Parse the trailer block of a commit message (`Key: value` lines at the end),
/// matching `git interpret-trailers`. Conservative to avoid false positives: the
/// block is the last paragraph, it must be preceded by a blank line (so a
/// subject-only message never parses as trailers), and EVERY line must be a
/// trailer (`token: value` / `token #value`), a continuation (leading whitespace),
/// or `(cherry picked from …)`. Returns (key, value) in order; empty if none.
fn parse_trailers(message: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(message);
    let lines: Vec<&str> = text.lines().collect();
    // Last paragraph = trailing run of non-blank lines.
    let end = lines.iter().rposition(|l| !l.trim().is_empty());
    let end = match end {
        Some(e) => e,
        None => return Vec::new(),
    };
    let mut start = end;
    while start > 0 && !lines[start - 1].trim().is_empty() {
        start -= 1;
    }
    // Must be separated from a body by a blank line (i.e. not the first paragraph).
    if start == 0 {
        return Vec::new();
    }
    let block = &lines[start..=end];
    let is_key = |l: &str| {
        // token = letters/digits/'-', then ": " or " #"
        let b = l.as_bytes();
        let n = b.iter().position(|&c| c == b':' || c == b'#');
        match n {
            Some(i) if i > 0 => b[..i].iter().all(|&c| c.is_ascii_alphanumeric() || c == b'-'),
            _ => false,
        }
    };
    let mut out: Vec<(String, String)> = Vec::new();
    for &l in block {
        if l.starts_with(' ') || l.starts_with('\t') {
            // continuation of the previous trailer's value
            if let Some(last) = out.last_mut() {
                last.1.push(' ');
                last.1.push_str(l.trim());
                continue;
            }
            return Vec::new(); // continuation with no trailer → not a trailer block
        }
        if l.starts_with("(cherry picked from") {
            continue; // git treats this as part of the block, not a trailer
        }
        if !is_key(l) {
            return Vec::new(); // a prose line → the block isn't trailers
        }
        // split on the first ':' or '#'
        let sep = l.bytes().position(|c| c == b':' || c == b'#').unwrap();
        let key = l[..sep].trim().to_string();
        let value = l[sep + 1..].trim().to_string();
        out.push((key, value));
    }
    out
}

fn tree_of(repo: &gix::Repository, id: &gix::ObjectId) -> Result<gix::ObjectId> {
    let commit = repo.find_commit(*id)?;
    let cref = commit.decode()?;
    Ok(gix::ObjectId::from_hex(cref.tree.as_ref())?)
}

/// Walk commits reachable from `tips` but not from `hidden` (empty = full history).
/// `cache_dir` () is a shared content-addressed blob-fact cache.
pub fn walk(
    repo: &gix::Repository,
    tips: Vec<gix::ObjectId>,
    hidden: Vec<gix::ObjectId>,
    cache_dir: Option<std::path::PathBuf>,
) -> Result<RawParts> {
    let zero = gix::ObjectId::null(repo.object_hash()).to_string();
    let mut commits = Vec::new();
    let mut messages = Vec::new();
    let mut trailers = Vec::new();
    let mut parents = Vec::new();
    let mut authors = Vec::new();
    let mut changes: Vec<(String, Change)> = Vec::new();
    let mut merge_changes: Vec<(String, Change)> = Vec::new();
    let mut stats = Stats::default();
    let has_cache = cache_dir.is_some();
    let mut sig_cache = crate::rename::SigCache::with_dir(cache_dir);
    let mut rstats = crate::rename::RenameStats::default();

    for info in repo.rev_walk(tips).with_hidden(hidden).all().context("rev walk")? {
        let info = info?;
        let id = info.id();
        let commit = repo.find_commit(id)?;
        let cref = commit.decode()?;
        let author = cref.author()?;
        let committer = cref.committer()?;
        let author_id = author.email.to_string().trim().to_lowercase();
        let committer_id = committer.email.to_string().trim().to_lowercase();
        let parent_ids: Vec<gix::ObjectId> = cref.parents().collect();
        let sha = id.to_string();

        for (i, p) in parent_ids.iter().enumerate() {
            parents.push(ParentRow { commit_sha: sha.clone(), parent_index: i as i32, parent_sha: p.to_string() });
        }

        let new_tree = gix::ObjectId::from_hex(cref.tree.as_ref())?;
        let old_tree = match parent_ids.first() {
            Some(p) => Some(tree_of(repo, p)?),
            None => None,
        };
        // Non-merge changes go to `changes` (→ commit_files, the churn/coupling
        // source). A MERGE's first-parent changes go to `merge_changes` instead, so
        // they don't inflate those aggregations — but "what did this merge bring"
        // stays queryable and first-parent history is complete.
        let mut cs = diff_commit(repo, old_tree, new_tree, &zero, &mut stats)?;
        crate::rename::detect_inexact(repo, &mut sig_cache, &mut cs, &mut rstats)?;
        let sink = if parent_ids.len() >= 2 { &mut merge_changes } else { &mut changes };
        for ch in cs {
            sink.push((sha.clone(), ch));
        }

        let msg = cref.message;
        let end = msg.iter().position(|&b| b == b'\n').unwrap_or(msg.len());
        for (i, (key, value)) in parse_trailers(msg).into_iter().enumerate() {
            trailers.push(TrailerRow { commit_sha: sha.clone(), seq: i as i32, key, value });
        }
        // Full message body (everything after the subject line), encoding, and
        // whether the commit carries a gpg signature (presence, not validity).
        let body = String::from_utf8_lossy(&msg[end..]).trim().to_string();
        messages.push(MessageRow {
            commit_sha: sha.clone(),
            body: (!body.is_empty()).then_some(body),
            encoding: cref.encoding.map(|e| e.to_string()),
            is_signed: cref.extra_headers.iter().any(|(k, _)| k.starts_with(b"gpgsig")),
        });
        commits.push(CommitRow {
            commit_sha: sha,
            author_id: author_id.clone(),
            authored_at_epoch: author.seconds(),
            // Author's tz offset (minutes east of UTC); 0 if the header is unparseable.
            authored_at_offset_minutes: author.time().map(|t| t.offset / 60).unwrap_or(0),
            committer_id: committer_id.clone(),
            committed_at_epoch: committer.seconds(),
            committed_at_offset_minutes: committer.time().map(|t| t.offset / 60).unwrap_or(0),
            subject: String::from_utf8_lossy(&msg[..end]).trim().to_string(),
            parent_count: parent_ids.len() as i32,
            is_merge: parent_ids.len() >= 2,
            is_root: parent_ids.is_empty(),
        });
        authors.push(AuthorRow { author_id, name: author.name.to_string(), email: author.email.to_string(), identity_id: 0 });
        // The committer is a person too — feed them into the identity graph so a
        // rebase/merge committer resolves like anyone else.
        authors.push(AuthorRow { author_id: committer_id, name: committer.name.to_string(), email: committer.email.to_string(), identity_id: 0 });
    }
    if has_cache {
        eprintln!(
            "  dedup cache: {} hits / {} misses (dedup_hit_rate {:.1}%)",
            sig_cache.hits, sig_cache.misses, sig_cache.hit_rate() * 100.0
        );
    }
    Ok(RawParts { commits, messages, trailers, parents, authors, changes, merge_changes })
}

/// Turn path-keyed raw parts + path-keyed blame + path-keyed symbols/refs into
/// the id-keyed L1/L3 tables.
pub fn assemble(
    parts: RawParts,
    blame_raw: Vec<BlameRow>,
    symbols_raw: Vec<SymbolRaw>,
    refs_raw: Vec<SymbolRefRaw>,
    content_generated: &HashSet<String>,
) -> Ingested {
    // Distinct paths (change new+old, blamed, symbol, reference) get ids.
    let mut paths: BTreeSet<String> = BTreeSet::new();
    for (_, ch) in &parts.changes {
        paths.insert(ch.path.clone());
        if let Some(op) = &ch.old_path {
            paths.insert(op.clone());
        }
    }
    for r in &blame_raw {
        paths.insert(r.path.clone());
    }
    for r in &symbols_raw {
        paths.insert(r.path.clone());
    }
    for r in &refs_raw {
        paths.insert(r.path.clone());
        if let Some(dp) = &r.def_path {
            paths.insert(dp.clone());
        }
    }
    let mut path_id: BTreeMap<String, i64> = BTreeMap::new();
    let mut files = Vec::with_capacity(paths.len());
    for (i, path) in paths.into_iter().enumerate() {
        let id = i as i64 + 1;
        path_id.insert(path.clone(), id);
        files.push(FileRow { file_id: id, path });
    }

    // Every distinct (email, name) ever seen — the co-occurrence signal for
    // identity resolution (an email that appeared under several names links them).
    let mut pairs: BTreeSet<(String, String)> = BTreeSet::new();
    for a in &parts.authors {
        pairs.insert((a.author_id.clone(), a.name.clone()));
    }
    // Pick a DETERMINISTIC representative (name,email) per author_id — the
    // lexicographic min — so the authors table is independent of walk order.
    let mut best: BTreeMap<String, (String, String)> = BTreeMap::new();
    for a in parts.authors {
        let cand = (a.name, a.email);
        best.entry(a.author_id)
            .and_modify(|e| { if cand < *e { *e = cand.clone(); } })
            .or_insert_with(|| cand.clone());
    }
    let mut authors: Vec<AuthorRow> = best
        .into_iter()
        .map(|(author_id, (name, email))| AuthorRow { author_id, name, email, identity_id: 0 })
        .collect();

    let (identities, identity_aliases, identity_reviews) = resolve_identities(&mut authors, &pairs);

    let mut commit_files = Vec::with_capacity(parts.changes.len());
    let mut hunks = Vec::new();
    for (commit_sha, ch) in parts.changes {
        let file_id = path_id[&ch.path];
        for (seq, h) in ch.hunks.iter().enumerate() {
            hunks.push(HunkRow {
                commit_sha: commit_sha.clone(),
                file_id,
                seq: seq as i32,
                old_start: h.old_start,
                old_lines: h.old_lines,
                new_start: h.new_start,
                new_lines: h.new_lines,
            });
        }
        commit_files.push(CommitFileRow {
            commit_sha,
            file_id,
            old_path_id: ch.old_path.as_ref().map(|p| path_id[p]),
            change_type: ch.change_type.to_string(),
            similarity: ch.similarity,
            added_lines: ch.added_lines,
            removed_lines: ch.removed_lines,
            src_blob_sha: ch.src_blob_sha,
            dst_blob_sha: ch.dst_blob_sha,
            src_mode: ch.src_mode,
            dst_mode: ch.dst_mode,
        });
    }

    // Merge commits' first-parent changes — path-keyed (a merge may touch historical
    // paths that never map to a HEAD file_id), so no path_id stitch is needed.
    let merge_changes: Vec<MergeChangeRow> = parts
        .merge_changes
        .into_iter()
        .map(|(commit_sha, ch)| MergeChangeRow {
            commit_sha,
            change_type: ch.change_type.to_string(),
            path: ch.path,
            old_path: ch.old_path,
            similarity: ch.similarity,
            added_lines: ch.added_lines,
            removed_lines: ch.removed_lines,
            src_blob_sha: ch.src_blob_sha,
            dst_blob_sha: ch.dst_blob_sha,
            src_mode: ch.src_mode,
            dst_mode: ch.dst_mode,
        })
        .collect();

    // Per-commit size (): fold commit_files into files/lines per commit.
    // Cheap (O(commits)) so it's always on, not tiered. Order-stable by commit_sha.
    let commit_stats = build_commit_stats(&commit_files);

    // Commit classification (): the KIND of each commit, from its subject.
    let commit_classes: Vec<CommitClassRow> = parts
        .commits
        .iter()
        .map(|c| {
            let (kind, scope, is_conventional, is_breaking) = classify_commit(&c.subject, c.is_merge);
            CommitClassRow { commit_sha: c.commit_sha.clone(), kind, scope, is_conventional, is_breaking }
        })
        .collect();

    let blame: Vec<BlameLineRow> = blame_raw
        .into_iter()
        .map(|r| BlameLineRow { file_id: path_id[&r.path], line_number: r.line_number, commit_sha: r.commit_sha })
        .collect();

    let level = Level::from_env();

    // Materialized ownership (.10b, mid+): fold HEAD blame into owned-line
    // counts per (file, resolved person). `parts.commits` is the full commit set in
    // both the full and incremental paths, so the blame→author→identity map is
    // complete here.
    let file_ownership = if level >= Level::Mid {
        materialize_ownership(&blame, &parts.commits, &authors)
    } else {
        Vec::new()
    };

    // Temporal coupling (, mid+): file pairs that co-change. Derived from
    // commit_files, so — like ownership — it's recomputed here in both index paths.
    let coupling = if level >= Level::Mid { build_coupling(&commit_files) } else { Vec::new() };

    // Collaboration (, mid+): person↔person by shared file edits,
    // identity-resolved, bots/hubs/bulk excluded.
    let collaboration = if level >= Level::Mid {
        build_collaboration(&commit_files, &parts.commits, &authors, &identities)
    } else {
        Vec::new()
    };

    // Files that are generated or vendored — excluded from the ownership rollups and
    // the briefing so tool-emitted code and checked-in deps don't skew "who owns /
    // what churns". Both sources of the generated_files table are honoured: the
    // path rule (classify) and the content markers the caller detected (mid+ blob
    // scan). The atomic tables (file_ownership, coupling) stay complete — only this
    // intelligence layer filters, so the raw facts remain queryable.
    let non_authored: HashSet<i64> = files
        .iter()
        .filter(|f| crate::generated::classify(&f.path).is_some() || content_generated.contains(&f.path))
        .map(|f| f.file_id)
        .collect();

    // Area ownership (, mid+): file_ownership rolled up to directories,
    // excluding generated/vendored files.
    let area_ownership = if level >= Level::Mid { build_area_ownership(&file_ownership, &files, &non_authored) } else { Vec::new() };

    let symbols: Vec<SymbolRow> = symbols_raw
        .into_iter()
        .map(|r| SymbolRow {
            file_id: path_id[&r.path],
            blob_sha: r.blob_sha,
            name: r.name,
            kind: r.kind,
            start_line: r.start_line,
            end_line: r.end_line,
            lang: r.lang,
        })
        .collect();

    let symbol_refs: Vec<SymbolRefRow> = refs_raw
        .into_iter()
        .map(|r| SymbolRefRow {
            file_id: path_id[&r.path],
            def_file_id: r.def_path.as_ref().map(|p| path_id[p]),
            blob_sha: r.blob_sha,
            name: r.name,
            ref_kind: r.ref_kind,
            line: r.line,
            lang: r.lang,
        })
        .collect();

    // Module dependency graph (): roll references up to file→file edges
    // by their resolved target. A pure fold of symbol_refs (empty at basic, where
    // there are no refs); recomputed here → incremental == full.
    let module_deps = build_module_deps(&symbol_refs);

    // Insights (, mid+): the briefing layer over the composite indices.
    // Placed after module_deps so architecture rules can read the file→file graph.
    let insights = if level >= Level::Mid {
        build_insights(&file_ownership, &area_ownership, &coupling, &commit_files, &files, &identities, &module_deps, &non_authored)
    } else {
        Vec::new()
    };

    // GRAIL (, high): attribute each reference to the definition that
    // encloses it and resolve the callee, yielding a symbol→symbol graph.
    let symbol_edges = if level == Level::High {
        build_symbol_edges(&symbols, &symbol_refs)
    } else {
        Vec::new()
    };

    Ingested {
        commits: parts.commits,
        refs: Vec::new(),         // computed in main (cheap, always current)
        notes: Vec::new(),        // computed in main (git notes)
        submodules: Vec::new(),   // computed in main (HEAD-derived)
        dependencies: Vec::new(), // computed in main (HEAD manifests)
        code_markers: Vec::new(),    // computed in main (HEAD content scan, mid+)
        secret_findings: Vec::new(), // computed in main (HEAD content scan, mid+)
        test_files: Vec::new(),      // computed in main (HEAD paths)
        test_coverage: Vec::new(),   // computed in main (name-based test→source)
        generated_files: Vec::new(), // computed in main (HEAD paths)
        blob_facts: Vec::new(),      // computed in main (content-addressed, mid+)
        tree_entries: Vec::new(), // computed in main (needs file_ids)
        commit_messages: parts.messages,
        commit_trailers: parts.trailers,
        parents: parts.parents,
        authors,
        files,
        commit_files,
        merge_changes,
        commit_stats,
        commit_classes,
        hunks,
        coupling,
        collaboration,
        area_ownership,
        insights,
        blame,
        blame_snapshots: Vec::new(),
        snapshot_ownership: Vec::new(), // computed in ingest_tips after snapshots
        file_ownership,
        identities,
        identity_aliases,
        identity_reviews,
        symbols,
        symbol_refs,
        module_deps,
        symbol_edges,
        chunks: Vec::new(),
        blob_chunks: Vec::new(),
        commit_trees: Vec::new(),
        tree_objects: Vec::new(),
    }
}

fn uf_find(parent: &mut [usize], x: usize) -> usize {
    let mut r = x;
    while parent[r] != r {
        r = parent[r];
    }
    let mut c = x; // path compression
    while parent[c] != r {
        let nxt = parent[c];
        parent[c] = r;
        c = nxt;
    }
    r
}

/// A GitHub noreply email encodes the login: `12345+login@users.noreply.github.com`
/// or `login@users.noreply.github.com`. That login is a strong, git-derivable
/// identity signal (no forge API needed).
fn github_login(email: &str) -> Option<String> {
    let e = email.to_lowercase();
    let local = e.strip_suffix("@users.noreply.github.com")?;
    let login = local.rsplit('+').next().unwrap_or(local);
    (!login.is_empty()).then(|| login.to_string())
}

fn token_jaccard(a: &str, b: &str) -> f64 {
    let ta: BTreeSet<&str> = a.split_whitespace().collect();
    let tb: BTreeSet<&str> = b.split_whitespace().collect();
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let inter = ta.intersection(&tb).count();
    let uni = ta.union(&tb).count();
    inter as f64 / uni as f64
}

///  — identity resolution by **union-find over git-derivable signals**.
/// Auto-merge on strong signals (same display name; same GitHub-noreply login);
/// weak signals (name similarity, shared email local-part) go to a **review
/// queue** (`identity_reviews`) as suggestions rather than automatic merges, so a
/// low-confidence guess never silently poisons a downstream join. Each alias
/// records the `method` and `confidence` that linked it. Forge-login-from-API
/// (SDR  signal 1) is out of scope here (needs the forge API / L2).
fn resolve_identities(
    authors: &mut [crate::model::AuthorRow],
    pairs: &BTreeSet<(String, String)>,
) -> (
    Vec<crate::model::IdentityRow>,
    Vec<crate::model::IdentityAliasRow>,
    Vec<crate::model::IdentityReviewRow>,
) {
    use crate::model::{IdentityAliasRow, IdentityRow};
    let n = authors.len();
    let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();

    // ── collect merge edges from each signal ──────────────────────────────────
    let mut edges: Vec<(usize, usize, &'static str, f64)> = Vec::new();
    {
        let email_ix: HashMap<&str, usize> =
            authors.iter().enumerate().map(|(i, a)| (a.author_id.as_str(), i)).collect();
        // Signal: an email used under a display name links every email that name.
        let mut name_emails: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
        for (email, name) in pairs {
            let nn = norm(name);
            if nn.is_empty() {
                continue;
            }
            if let Some(&ix) = email_ix.get(email.as_str()) {
                name_emails.entry(nn).or_default().insert(ix);
            }
        }
        for set in name_emails.values() {
            let v: Vec<usize> = set.iter().copied().collect();
            for w in v.windows(2) {
                edges.push((w[0], w[1], "name-exact", 0.9));
            }
        }
        // Signal: same GitHub-noreply login.
        let mut login_emails: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
        for (i, a) in authors.iter().enumerate() {
            if let Some(login) = github_login(&a.email) {
                login_emails.entry(login).or_default().insert(i);
            }
        }
        for set in login_emails.values() {
            let v: Vec<usize> = set.iter().copied().collect();
            for w in v.windows(2) {
                edges.push((w[0], w[1], "forge-noreply", 0.95));
            }
        }
    }

    // ── union-find + per-author linking method / cluster confidence ───────────
    let mut parent: Vec<usize> = (0..n).collect();
    for &(a, b, _, _) in &edges {
        let (ra, rb) = (uf_find(&mut parent, a), uf_find(&mut parent, b));
        if ra != rb {
            parent[ra] = rb;
        }
    }
    // strongest incident edge per author → its alias method/confidence.
    let mut has_edge = vec![false; n];
    let mut a_method = vec!["sole"; n];
    let mut a_conf = vec![1.0f64; n];
    for &(a, b, m, c) in &edges {
        for x in [a, b] {
            if !has_edge[x] || c > a_conf[x] {
                has_edge[x] = true;
                a_method[x] = m;
                a_conf[x] = c;
            }
        }
    }
    // cluster confidence = weakest edge that formed it (1.0 for singletons).
    let mut cluster_conf: HashMap<usize, f64> = HashMap::new();
    for &(a, _, _, c) in &edges {
        let r = uf_find(&mut parent, a);
        let e = cluster_conf.entry(r).or_insert(1.0);
        *e = e.min(c);
    }

    // ── build identities (deterministic ids by canonical email) ───────────────
    let mut members: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let r = uf_find(&mut parent, i);
        members.entry(r).or_default().push(i);
    }
    let mut clusters: Vec<(String, String, Vec<usize>, f64)> = members
        .into_iter()
        .map(|(root, mem)| {
            let email = mem.iter().map(|&i| authors[i].email.clone()).min().unwrap();
            let name = mem.iter().map(|&i| authors[i].name.clone()).min().unwrap();
            let confidence = *cluster_conf.get(&root).unwrap_or(&1.0);
            (email, name, mem, confidence)
        })
        .collect();
    clusters.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    let mut identities = Vec::with_capacity(clusters.len());
    let mut aliases = Vec::with_capacity(n);
    for (k, (email, name, mem, confidence)) in clusters.into_iter().enumerate() {
        let identity_id = k as i64 + 1;
        for &i in &mem {
            authors[i].identity_id = identity_id;
            aliases.push(IdentityAliasRow {
                identity_id,
                author_id: authors[i].author_id.clone(),
                name: authors[i].name.clone(),
                email: authors[i].email.clone(),
                method: a_method[i].to_string(),
                confidence: a_conf[i],
            });
        }
        identities.push(IdentityRow { identity_id, name, email, confidence, alias_count: mem.len() as i32 });
    }

    // ── review queue: weak signals as SUGGESTIONS, never auto-merged ──────────
    let reviews = build_review_queue(&identities, &aliases, &norm);
    (identities, aliases, reviews)
}

/// Suggest merges (name similarity, shared non-generic email local-part) between
/// distinct identities without applying them — the review queue.
fn build_review_queue(
    identities: &[crate::model::IdentityRow],
    aliases: &[crate::model::IdentityAliasRow],
    norm: &impl Fn(&str) -> String,
) -> Vec<crate::model::IdentityReviewRow> {
    use crate::model::IdentityReviewRow;
    let order2 = |a: i64, b: i64| if a < b { (a, b) } else { (b, a) };
    let mut seen: HashSet<(i64, i64)> = HashSet::new();
    let mut reviews: Vec<IdentityReviewRow> = Vec::new();
    let name_of: HashMap<i64, &str> = identities.iter().map(|r| (r.identity_id, r.name.as_str())).collect();

    // name-similar, blocked by first name token to stay tractable.
    let mut blocks: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (idx, id) in identities.iter().enumerate() {
        let nn = norm(&id.name);
        if let Some(first) = nn.split_whitespace().next() {
            if !first.is_empty() {
                blocks.entry(first.to_string()).or_default().push(idx);
            }
        }
    }
    for idxs in blocks.values() {
        if idxs.len() < 2 || idxs.len() > 60 {
            continue; // skip huge blocks (e.g. bots) — O(block²) stays small
        }
        for i in 0..idxs.len() {
            for j in (i + 1)..idxs.len() {
                let (ia, ib) = (idxs[i], idxs[j]);
                let sim = token_jaccard(&norm(&identities[ia].name), &norm(&identities[ib].name));
                if sim >= 0.5 && sim < 1.0 {
                    let (a, b) = order2(identities[ia].identity_id, identities[ib].identity_id);
                    if seen.insert((a, b)) {
                        reviews.push(IdentityReviewRow {
                            identity_a: a,
                            identity_b: b,
                            name_a: identities[ia].name.clone(),
                            name_b: identities[ib].name.clone(),
                            reason: "name-similar".to_string(),
                            similarity: (sim * 1000.0).round() / 1000.0,
                        });
                    }
                }
            }
        }
    }

    // shared non-generic email local-part across identities.
    let generic: HashSet<&str> = [
        "git", "admin", "root", "info", "ci", "noreply", "no-reply", "dev", "hello", "me",
        "mail", "user", "github", "action", "actions", "bot", "support", "team", "build",
    ].into_iter().collect();
    let mut local_ids: BTreeMap<String, BTreeSet<i64>> = BTreeMap::new();
    for al in aliases {
        if let Some((local, _)) = al.email.split_once('@') {
            let l = local.to_lowercase();
            if l.len() >= 3 && !generic.contains(l.as_str()) {
                local_ids.entry(l).or_default().insert(al.identity_id);
            }
        }
    }
    for ids in local_ids.values() {
        if ids.len() < 2 {
            continue;
        }
        let v: Vec<i64> = ids.iter().copied().collect();
        for i in 0..v.len() {
            for j in (i + 1)..v.len() {
                let (a, b) = order2(v[i], v[j]);
                if seen.insert((a, b)) {
                    reviews.push(IdentityReviewRow {
                        identity_a: a,
                        identity_b: b,
                        name_a: name_of.get(&a).copied().unwrap_or("").to_string(),
                        name_b: name_of.get(&b).copied().unwrap_or("").to_string(),
                        reason: "email-local".to_string(),
                        similarity: 0.7,
                    });
                }
            }
        }
    }

    reviews.sort_by(|x, y| x.identity_a.cmp(&y.identity_a).then(x.identity_b.cmp(&y.identity_b)));
    reviews
}

/// Fold HEAD blame into per-(file, person) owned-line counts and shares. A line
/// is attributed to the resolved identity of its origin commit's author; a line
/// whose origin commit isn't in the commit set (a shallow/graft boundary) is
/// counted in `file_lines` but attributed to no one, so a file's shares sum to
/// ≤ 1 (the shortfall is the unattributable fraction, ~0 on a full history).
/// Binary/empty files carry no blame, so they simply produce no rows. Order is
/// deterministic: (file_id, then identity_id).
/// Ownership over time (): roll snapshot blame up to repo-wide owned
/// lines per (snapshot, resolved person). Empty when no snapshots were taken.
fn build_snapshot_ownership(
    snapshots: &[SnapshotBlameRow],
    commits: &[CommitRow],
    authors: &[AuthorRow],
) -> Vec<SnapshotOwnershipRow> {
    if snapshots.is_empty() {
        return Vec::new();
    }
    let author_identity: HashMap<&str, i64> =
        authors.iter().map(|a| (a.author_id.as_str(), a.identity_id)).collect();
    let commit_identity: HashMap<&str, i64> = commits
        .iter()
        .filter_map(|c| author_identity.get(c.author_id.as_str()).map(|&id| (c.commit_sha.as_str(), id)))
        .collect();
    // Key snapshots by (sha, ref): a sha under two refs is two series.
    let mut owned: BTreeMap<(String, String, i64), i64> = BTreeMap::new();
    let mut total: BTreeMap<(String, String), i64> = BTreeMap::new();
    for s in snapshots {
        let key = (s.snapshot_sha.clone(), s.snapshot_ref.clone());
        *total.entry(key.clone()).or_default() += 1;
        if let Some(&iid) = commit_identity.get(s.commit_sha.as_str()) {
            *owned.entry((s.snapshot_sha.clone(), s.snapshot_ref.clone(), iid)).or_default() += 1;
        }
    }
    owned
        .into_iter()
        .map(|((sha, sref, iid), lines)| {
            let t = total[&(sha.clone(), sref.clone())];
            SnapshotOwnershipRow {
                snapshot_ref: sref,
                snapshot_sha: sha,
                identity_id: iid,
                owned_lines: lines,
                total_lines: t,
                ownership_share: lines as f64 / t as f64,
            }
        })
        .collect()
}

fn materialize_ownership(
    blame: &[BlameLineRow],
    commits: &[CommitRow],
    authors: &[AuthorRow],
) -> Vec<FileOwnershipRow> {
    let author_identity: HashMap<&str, i64> =
        authors.iter().map(|a| (a.author_id.as_str(), a.identity_id)).collect();
    let commit_identity: HashMap<&str, i64> = commits
        .iter()
        .filter_map(|c| author_identity.get(c.author_id.as_str()).map(|&id| (c.commit_sha.as_str(), id)))
        .collect();

    let mut owned: BTreeMap<(i64, i64), i64> = BTreeMap::new(); // (file_id, identity_id) -> lines
    let mut file_lines: BTreeMap<i64, i64> = BTreeMap::new();
    for b in blame {
        *file_lines.entry(b.file_id).or_default() += 1;
        if let Some(&iid) = commit_identity.get(b.commit_sha.as_str()) {
            *owned.entry((b.file_id, iid)).or_default() += 1;
        }
    }
    owned
        .into_iter()
        .map(|((file_id, identity_id), lines)| FileOwnershipRow {
            file_id,
            identity_id,
            owned_lines: lines,
            file_lines: file_lines[&file_id],
            ownership_share: lines as f64 / file_lines[&file_id] as f64,
        })
        .collect()
}

/// Per-commit size (): fold commit_files into files-changed + lines
/// added/removed per commit. Binary changes have null line counts (like git) and
/// count 0 lines but still count as a changed file. Sorted by commit_sha for a
/// reproducible order.
fn build_commit_stats(commit_files: &[CommitFileRow]) -> Vec<CommitStatRow> {
    let mut by: BTreeMap<&str, (i32, i64, i64)> = BTreeMap::new(); // (files, ins, del)
    for cf in commit_files {
        let e = by.entry(cf.commit_sha.as_str()).or_insert((0, 0, 0));
        e.0 += 1;
        e.1 += cf.added_lines.unwrap_or(0) as i64;
        e.2 += cf.removed_lines.unwrap_or(0) as i64;
    }
    by.into_iter()
        .map(|(sha, (files, ins, del))| CommitStatRow {
            commit_sha: sha.to_string(),
            files_changed: files,
            insertions: ins,
            deletions: del,
            net_lines: ins - del,
        })
        .collect()
}

/// Module dependency graph (): fold `symbol_refs` into file→file edges
/// by resolved target. from_file uses to_file `ref_count` times. Self-edges and
/// unresolved refs (no def_file_id) are dropped. Deterministic order.
fn build_module_deps(symbol_refs: &[SymbolRefRow]) -> Vec<ModuleDepRow> {
    let mut counts: HashMap<(i64, i64), i64> = HashMap::new();
    for r in symbol_refs {
        if let Some(to) = r.def_file_id {
            if to != r.file_id {
                *counts.entry((r.file_id, to)).or_default() += 1;
            }
        }
    }
    let mut out: Vec<ModuleDepRow> = counts
        .into_iter()
        .map(|((from, to), n)| ModuleDepRow { from_file_id: from, to_file_id: to, ref_count: n })
        .collect();
    out.sort_by(|a, b| a.from_file_id.cmp(&b.from_file_id).then(a.to_file_id.cmp(&b.to_file_id)));
    out
}

/// Temporal coupling (): count how often each unordered file pair
/// changes in the same commit, excluding bulk commits (a mass move/reformat that
/// would couple everything). Keyed by `file_id` (a<b); pairs below the min
/// co-change threshold are dropped. Deterministic order (co_changes desc, then
/// ids) so output is reproducible.
fn build_coupling(commit_files: &[CommitFileRow]) -> Vec<CouplingRow> {
    // file_ids touched per commit.
    let mut by_commit: HashMap<&str, Vec<i64>> = HashMap::new();
    for cf in commit_files {
        by_commit.entry(cf.commit_sha.as_str()).or_default().push(cf.file_id);
    }
    let mut counts: HashMap<(i64, i64), i64> = HashMap::new();
    for files in by_commit.values() {
        if files.len() > BULK_COMMIT_MAX_FILES {
            continue; // bulk commit — not meaningful coupling
        }
        let mut fs = files.clone();
        fs.sort_unstable();
        fs.dedup(); // one file appears once per commit; belt-and-braces
        for i in 0..fs.len() {
            for j in (i + 1)..fs.len() {
                *counts.entry((fs[i], fs[j])).or_default() += 1;
            }
        }
    }
    let mut out: Vec<CouplingRow> = counts
        .into_iter()
        .filter(|(_, n)| *n >= COUPLING_MIN_COCHANGES)
        .map(|((a, b), n)| CouplingRow { file_a_id: a, file_b_id: b, co_changes: n })
        .collect();
    out.sort_by(|x, y| {
        y.co_changes
            .cmp(&x.co_changes)
            .then(x.file_a_id.cmp(&y.file_a_id))
            .then(x.file_b_id.cmp(&y.file_b_id))
    });
    out
}

/// The "area" of a file — its immediate parent directory, "." for repo root.
/// One canonical rule so every consumer rolls up ownership the same way.
fn area_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => ".",
    }
}

/// Area ownership (): roll HEAD `file_ownership` up from files to their
/// directory. owned = Σ owned_lines of the person's files in the area; area_lines =
/// Σ file_lines over the DISTINCT files in the area (each file's blame total once).
fn build_area_ownership(fo: &[FileOwnershipRow], files: &[FileRow], non_authored: &HashSet<i64>) -> Vec<AreaOwnershipRow> {
    let id2path: HashMap<i64, &str> = files.iter().map(|f| (f.file_id, f.path.as_str())).collect();
    let mut owned: BTreeMap<(String, i64), i64> = BTreeMap::new();
    let mut area_lines: BTreeMap<String, i64> = BTreeMap::new();
    let mut counted: std::collections::HashSet<i64> = std::collections::HashSet::new();
    for r in fo {
        if non_authored.contains(&r.file_id) {
            continue; // generated/vendored — not human-authored, excluded from ownership
        }
        let area = area_of(id2path[&r.file_id]).to_string();
        *owned.entry((area.clone(), r.identity_id)).or_default() += r.owned_lines;
        if counted.insert(r.file_id) {
            *area_lines.entry(area).or_default() += r.file_lines;
        }
    }
    owned
        .into_iter()
        .map(|((area, identity_id), lines)| {
            let al = area_lines[&area];
            AreaOwnershipRow {
                area,
                identity_id,
                owned_lines: lines,
                area_lines: al,
                ownership_share: lines as f64 / al as f64,
            }
        })
        .collect()
}

/// Insights (): the briefing layer. Turn the composite indices into
/// typed, human-readable findings. Each rule is definitional (its rows are exactly
/// the rule's query), so the bench can check it. Sorted by severity then metric so
/// a reader sees the most important findings first.
fn build_insights(
    file_ownership: &[FileOwnershipRow],
    area_ownership: &[AreaOwnershipRow],
    coupling: &[CouplingRow],
    commit_files: &[CommitFileRow],
    files: &[FileRow],
    identities: &[IdentityRow],
    module_deps: &[ModuleDepRow],
    non_authored: &HashSet<i64>,
) -> Vec<InsightRow> {
    let id2path: HashMap<i64, &str> = files.iter().map(|f| (f.file_id, f.path.as_str())).collect();
    let iid2name: HashMap<i64, &str> = identities.iter().map(|i| (i.identity_id, i.name.as_str())).collect();
    let pct = |s: f64| (s * 100.0).round() as i64;
    let who = |iid: i64| iid2name.get(&iid).copied().unwrap_or("(unknown)");
    // Generated/vendored files are not human-authored: they carry no meaningful
    // ownership or churn signal, so every rule below skips them (a lockfile is not
    // a "hotspot" or a "bus-factor risk"). area_key_person reads area_ownership,
    // which is already filtered, so it needs no extra guard.
    let authored = |id: i64| !non_authored.contains(&id);
    let mut out: Vec<InsightRow> = Vec::new();

    // Keep the top-N of a candidate set by a salience key (desc), tie-broken by
    // subject (asc) so the cut is a total, reproducible order — a briefing digest.
    fn top_n(mut cands: Vec<(f64, InsightRow)>) -> Vec<InsightRow> {
        cands.sort_by(|a, b| {
            b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal).then(a.1.subject.cmp(&b.1.subject))
        });
        cands.truncate(INSIGHTS_TOP_N);
        cands.into_iter().map(|(_, r)| r).collect()
    }

    // 0) codebase_bus_factor — ONE org-level headline: the single person who owns
    //    the most of the whole codebase, and their share. On a solo repo this is
    //    the real story (instead of a per-file flood).
    let mut owned_by: HashMap<i64, i64> = HashMap::new();
    let mut total_lines: i64 = 0;
    let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::new();
    for r in file_ownership {
        if !authored(r.file_id) {
            continue; // generated/vendored — excluded from the ownership headline
        }
        *owned_by.entry(r.identity_id).or_default() += r.owned_lines;
        if seen.insert(r.file_id) {
            total_lines += r.file_lines;
        }
    }
    if let (Some((&top_id, &top_lines)), true) = (owned_by.iter().max_by_key(|(_, &v)| v), total_lines > 0) {
        let share = top_lines as f64 / total_lines as f64;
        let sev = if share >= 0.80 { "critical" } else if share >= 0.50 { "warning" } else { "info" };
        out.push(InsightRow {
            kind: "codebase_bus_factor".into(),
            severity: sev.into(),
            subject: "repository".into(),
            metric: share,
            detail: format!("{} owns {}% of the codebase ({} of {} lines) — the single largest knowledge concentration", who(top_id), pct(share), top_lines, total_lines),
        });
    }

    // 1) bus_factor_risk — the biggest HEAD files owned almost entirely by one
    //    person (top N by size, so the reader sees the files that matter most).
    let bus: Vec<(f64, InsightRow)> = file_ownership
        .iter()
        .filter(|r| authored(r.file_id) && r.file_lines >= BUS_MIN_LINES && r.ownership_share >= BUS_CRIT_SHARE)
        .map(|r| {
            let path = id2path[&r.file_id];
            (r.file_lines as f64, InsightRow {
                kind: "bus_factor_risk".into(),
                severity: "critical".into(),
                subject: path.to_string(),
                metric: r.ownership_share,
                detail: format!("{} owns {}% of {path} ({} lines) — single-person risk", who(r.identity_id), pct(r.ownership_share), r.file_lines),
            })
        })
        .collect();
    out.extend(top_n(bus));

    // 2) area_key_person — the biggest modules one person owns (top N by size).
    let areas: Vec<(f64, InsightRow)> = area_ownership
        .iter()
        .filter(|r| r.area_lines >= AREA_MIN_LINES && r.ownership_share >= AREA_KEY_SHARE)
        .map(|r| {
            (r.area_lines as f64, InsightRow {
                kind: "area_key_person".into(),
                severity: "warning".into(),
                subject: r.area.clone(),
                metric: r.ownership_share,
                detail: format!("{} owns {}% of {}/ ({} lines) — the module depends on one person", who(r.identity_id), pct(r.ownership_share), r.area, r.area_lines),
            })
        })
        .collect();
    out.extend(top_n(areas));

    // 3) hotspot — the most-changed files (top N by churn).
    let mut churn: HashMap<i64, i64> = HashMap::new();
    for cf in commit_files {
        if !authored(cf.file_id) {
            continue; // generated/vendored churn (a lockfile) is not a real hotspot
        }
        *churn.entry(cf.file_id).or_default() += 1;
    }
    let hot: Vec<(f64, InsightRow)> = churn
        .iter()
        .filter(|(_, &n)| n >= HOTSPOT_MIN_CHANGES)
        .map(|(&file_id, &n)| {
            let path = id2path[&file_id];
            (n as f64, InsightRow {
                kind: "hotspot".into(),
                severity: "info".into(),
                subject: path.to_string(),
                metric: n as f64,
                detail: format!("{path} changed {n}× — a hotspot; churn concentrates risk and review load"),
            })
        })
        .collect();
    out.extend(top_n(hot));

    // 4) hidden_coupling — the strongest CROSS-module coupled pairs (top N by co).
    let hidden: Vec<(f64, InsightRow)> = coupling
        .iter()
        .filter(|c| c.co_changes >= HIDDEN_COUPLING_MIN)
        .filter(|c| authored(c.file_a_id) && authored(c.file_b_id))
        .filter_map(|c| {
            let (pa, pb) = (id2path[&c.file_a_id], id2path[&c.file_b_id]);
            if area_of(pa) == area_of(pb) {
                return None; // same module — expected, not "hidden"
            }
            Some((c.co_changes as f64, InsightRow {
                kind: "hidden_coupling".into(),
                severity: "info".into(),
                subject: format!("{pa} ↔ {pb}"),
                metric: c.co_changes as f64,
                detail: format!("{pa} and {pb} change together {}× across modules — a hidden dependency", c.co_changes),
            }))
        })
        .collect();
    out.extend(top_n(hidden));

    // 5) architecture_hub — files many others depend on (module_deps in-degree).
    //    A change to a hub ripples widely, so its blast radius is a review signal.
    //    ≥ 2× the threshold escalates info → warning.
    let mut in_degree: HashMap<i64, i64> = HashMap::new();
    for e in module_deps {
        if !authored(e.to_file_id) {
            continue; // a generated/vendored file is not an authored architecture hub
        }
        *in_degree.entry(e.to_file_id).or_default() += 1;
    }
    let hubs: Vec<(f64, InsightRow)> = in_degree
        .iter()
        .filter(|(_, &n)| n >= HUB_MIN_DEPENDENTS)
        .map(|(&file_id, &n)| {
            let path = id2path[&file_id];
            let sev = if n >= HUB_MIN_DEPENDENTS * 2 { "warning" } else { "info" };
            (n as f64, InsightRow {
                kind: "architecture_hub".into(),
                severity: sev.into(),
                subject: path.to_string(),
                metric: n as f64,
                detail: format!("{n} files depend on {path} — a change here has wide blast radius"),
            })
        })
        .collect();
    out.extend(top_n(hubs));

    // 6) fragile_hub — a file that is BOTH a hub (high in-degree) AND a hotspot
    //    (high churn): structural centrality times volatility. Neither table alone
    //    surfaces it; the intersection is the single riskiest kind of file to touch.
    let fragile: Vec<(f64, InsightRow)> = in_degree
        .iter()
        .filter(|(_, &deg)| deg >= HUB_MIN_DEPENDENTS)
        .filter_map(|(&file_id, &deg)| {
            let n = *churn.get(&file_id)?;
            if n < FRAGILE_HUB_MIN_CHANGES {
                return None;
            }
            let path = id2path[&file_id];
            // Salience = dependents × changes: wide blast radius meets high volatility.
            Some(((deg * n) as f64, InsightRow {
                kind: "fragile_hub".into(),
                severity: "warning".into(),
                subject: path.to_string(),
                metric: (deg * n) as f64,
                detail: format!("{path} is a hub ({deg} dependents) and changed {n}× — high-blast-radius churn"),
            }))
        })
        .collect();
    out.extend(top_n(fragile));

    // Most important first: severity rank, then the driving metric, deterministic.
    let rank = |s: &str| match s { "critical" => 0, "warning" => 1, _ => 2 };
    out.sort_by(|x, y| {
        rank(&x.severity)
            .cmp(&rank(&y.severity))
            .then(y.metric.partial_cmp(&x.metric).unwrap_or(std::cmp::Ordering::Equal))
            .then(x.kind.cmp(&y.kind))
            .then(x.subject.cmp(&y.subject))
    });
    out
}

/// Collaboration (): person↔person edges weighted by how many files
/// both edited. Identity-resolved; excludes bulk commits, bot authors, and hub
/// files (touched by too many people). `strength` is the Jaccard of the two
/// people's file sets. Deterministic order (shared desc, then ids).
fn build_collaboration(
    commit_files: &[CommitFileRow],
    commits: &[CommitRow],
    authors: &[AuthorRow],
    identities: &[IdentityRow],
) -> Vec<CollaborationRow> {
    // commit_sha -> resolved identity of its author.
    let author_identity: HashMap<&str, i64> =
        authors.iter().map(|a| (a.author_id.as_str(), a.identity_id)).collect();
    let commit_identity: HashMap<&str, i64> = commits
        .iter()
        .filter_map(|c| author_identity.get(c.author_id.as_str()).map(|&id| (c.commit_sha.as_str(), id)))
        .collect();
    let bots: std::collections::HashSet<i64> = identities
        .iter()
        .filter(|i| is_bot_identity(&i.name, &i.email))
        .map(|i| i.identity_id)
        .collect();

    // Bulk commits (> BULK_COMMIT_MAX_FILES) are excluded.
    let mut files_per_commit: HashMap<&str, usize> = HashMap::new();
    for cf in commit_files {
        *files_per_commit.entry(cf.commit_sha.as_str()).or_default() += 1;
    }

    // Distinct (file, person) over non-bulk commits, excluding bots.
    let mut touched: std::collections::HashSet<(i64, i64)> = std::collections::HashSet::new();
    for cf in commit_files {
        if files_per_commit[cf.commit_sha.as_str()] > BULK_COMMIT_MAX_FILES {
            continue;
        }
        if let Some(&iid) = commit_identity.get(cf.commit_sha.as_str()) {
            if !bots.contains(&iid) {
                touched.insert((cf.file_id, iid));
            }
        }
    }

    // Editors per file → drop hub files (touched by too many people).
    let mut editors_of: HashMap<i64, Vec<i64>> = HashMap::new();
    for &(file_id, iid) in &touched {
        editors_of.entry(file_id).or_default().push(iid);
    }
    // person's file count (over kept, non-hub files), and pair co-edit counts.
    let mut person_files: HashMap<i64, i64> = HashMap::new();
    let mut pair_shared: HashMap<(i64, i64), i64> = HashMap::new();
    for eds in editors_of.values_mut() {
        if eds.len() > COLLAB_MAX_EDITORS {
            continue; // hub file
        }
        eds.sort_unstable();
        eds.dedup();
        for &iid in eds.iter() {
            *person_files.entry(iid).or_default() += 1;
        }
        for i in 0..eds.len() {
            for j in (i + 1)..eds.len() {
                *pair_shared.entry((eds[i], eds[j])).or_default() += 1;
            }
        }
    }

    let mut out: Vec<CollaborationRow> = pair_shared
        .into_iter()
        .map(|((a, b), shared)| {
            let (af, bf) = (person_files[&a], person_files[&b]);
            let union = af + bf - shared;
            CollaborationRow {
                identity_a: a,
                identity_b: b,
                shared_files: shared,
                a_files: af,
                b_files: bf,
                strength: if union > 0 { shared as f64 / union as f64 } else { 0.0 },
            }
        })
        .collect();
    out.sort_by(|x, y| {
        y.shared_files
            .cmp(&x.shared_files)
            .then(x.identity_a.cmp(&y.identity_a))
            .then(x.identity_b.cmp(&y.identity_b))
    });
    out
}

/// GRAIL (): build the symbol reference graph from `symbols` +
/// `symbol_refs`. Each reference is attributed to the definition that ENCLOSES its
/// call site (innermost symbol in the same file whose line range contains the ref
/// — so a call inside a method binds to that method, not its class), and the
/// callee is resolved to a single symbol when its (resolved-file, name) is
/// unambiguous. References not inside any definition (module/top-level scope) have
/// no caller symbol and are excluded — the table is a symbol→symbol graph. Lexical,
/// not semantic; `ref_kind` is carried so consumers can gate on `call`/`new`.
fn build_symbol_edges(symbols: &[SymbolRow], refs: &[SymbolRefRow]) -> Vec<SymbolEdgeRow> {
    let mut by_file: HashMap<i64, Vec<&SymbolRow>> = HashMap::new();
    for s in symbols {
        by_file.entry(s.file_id).or_default().push(s);
    }
    // (file_id, name) -> start lines of symbols with that name, for callee resolution.
    let mut by_name: HashMap<(i64, &str), Vec<i32>> = HashMap::new();
    for s in symbols {
        by_name.entry((s.file_id, s.name.as_str())).or_default().push(s.start_line);
    }

    let mut edges = Vec::new();
    for r in refs {
        // Innermost enclosing definition: contains the line, smallest span, and on a
        // tie the deeper (later-starting) one.
        let src = match by_file.get(&r.file_id).and_then(|syms| {
            syms.iter()
                .filter(|s| s.start_line <= r.line && r.line <= s.end_line)
                .min_by_key(|s| (s.end_line - s.start_line, -s.start_line))
        }) {
            Some(s) => *s,
            None => continue, // module-scope reference: no caller symbol
        };
        // Callee: resolved to a single symbol only when (resolved file, name) is unique.
        let dst_start_line = r.def_file_id.and_then(|fid| match by_name.get(&(fid, r.name.as_str())) {
            Some(starts) if starts.len() == 1 => Some(starts[0]),
            _ => None,
        });
        edges.push(SymbolEdgeRow {
            src_file_id: src.file_id,
            src_name: src.name.clone(),
            src_kind: src.kind.clone(),
            src_start_line: src.start_line,
            dst_name: r.name.clone(),
            dst_file_id: r.def_file_id,
            dst_start_line,
            ref_kind: r.ref_kind.clone(),
            line: r.line,
            lang: r.lang.clone(),
        });
    }
    edges.sort_by(|a, b| {
        a.src_file_id
            .cmp(&b.src_file_id)
            .then(a.src_start_line.cmp(&b.src_start_line))
            .then(a.line.cmp(&b.line))
            .then(a.dst_name.cmp(&b.dst_name))
            .then(a.ref_kind.cmp(&b.ref_kind))
    });
    edges
}

/// Full index over an explicit set of ref tips (). Blame is always
/// for the current HEAD (the default branch's working set).
pub fn ingest_tips(repo_path: &Path, tips: Vec<gix::ObjectId>, cache_dir: Option<std::path::PathBuf>) -> Result<Ingested> {
    let repo = gix::discover(repo_path).with_context(|| format!("opening {repo_path:?}"))?;
    let parts = walk(&repo, tips, vec![], cache_dir)?;
    // git-CLI blame is the default: 100% agreement, and measured faster than the
    // native path at scale (git's C blame beats gitoxide's per-file walk on large
    // repos, offsetting the subprocess savings). EOS_BLAME=native opts into the
    // in-process gitoxide blame (≥99.5%, identical coverage; faster on small repos,
    // no git binary — see README).
    // "Single-add" files (added in exactly one commit, never touched since) have a
    // trivial blame — every line is that add commit — so compute_blame can skip the
    // git blame subprocess for them (~half the files on a typical repo). Build the
    // path→add-commit map from the walk's changes: a path that appears in exactly
    // one change, of type 'A'. (Renamed-away paths are excluded by compute_blame
    // intersecting with the HEAD file set.)
    let mut change_of: HashMap<&str, (u32, &str, char)> = HashMap::new();
    for (sha, ch) in &parts.changes {
        let e = change_of.entry(ch.path.as_str()).or_insert((0, "", ' '));
        e.0 += 1;
        e.1 = sha.as_str();
        e.2 = ch.change_type;
    }
    let single_add: HashMap<String, String> = change_of
        .into_iter()
        .filter(|(_, (n, _, ct))| *n == 1 && *ct == 'A')
        .map(|(p, (_, sha, _))| (p.to_string(), sha.to_string()))
        .collect();

    let blame_raw = if std::env::var("EOS_BLAME").as_deref() == Ok("native") {
        compute_blame_native(repo_path)?
    } else {
        compute_blame(repo_path, &single_add)?
    };
    let level = Level::from_env();
    // L3 symbols/references are a mid+ tier; basic skips parsing entirely.
    let (symbols_raw, refs_raw) = if level >= Level::Mid { compute_l3(repo_path)? } else { (Vec::new(), Vec::new()) };
    // Content-marker generated files (mid+ blob scan) feed the ownership/insights
    // exclusion — the path rule alone can't see a `@generated` header in a normally
    // named file. Same scan the generated_files table uses in `main`.
    let content_generated: HashSet<String> = if level >= Level::Mid {
        crate::generated::compute_content(repo_path)?.into_iter().collect()
    } else {
        HashSet::new()
    };
    eprintln!(
        "  full index [{}]: {} commits, {} changes, {} blame lines, {} symbols, {} refs",
        level.tag(), parts.commits.len(), parts.changes.len(), blame_raw.len(), symbols_raw.len(), refs_raw.len()
    );
    let mut ing = assemble(parts, blame_raw, symbols_raw, refs_raw, &content_generated);
    // Historical blame snapshots (.7b) — opt-in via EOS_SNAPSHOTS, and on by
    // default (tags) at the `high` tier. Empty and free otherwise. Full-index-only
    // (a push preserves prior ones). Chunk dedup () is the same story.
    let high = level == Level::High;
    ing.blame_snapshots = crate::snapshots::compute(repo_path, high)?;
    // Ownership over time: roll the snapshot blame up per (snapshot, person).
    ing.snapshot_ownership = build_snapshot_ownership(&ing.blame_snapshots, &ing.commits, &ing.authors);
    let (chunks, blob_chunks) = crate::chunk::compute(repo_path, high)?;
    ing.chunks = chunks;
    ing.blob_chunks = blob_chunks;
    // Historical tree objects () — the lossless directory layer. On at
    // `high`, or opt-in via EOS_TREES. Full-index-only (a push preserves prior ones).
    let want_trees = high || std::env::var("EOS_TREES").map(|v| v != "0").unwrap_or(false);
    if want_trees {
        let shas: Vec<String> = ing.commits.iter().map(|c| c.commit_sha.clone()).collect();
        let (commit_trees, tree_objects) = crate::trees::compute(repo_path, &shas)?;
        ing.commit_trees = commit_trees;
        ing.tree_objects = tree_objects;
    }
    Ok(ing)
}

/// Full index of the default branch (HEAD).
pub fn ingest(repo_path: &Path, cache_dir: Option<std::path::PathBuf>) -> Result<Ingested> {
    let repo = gix::discover(repo_path).with_context(|| format!("opening {repo_path:?}"))?;
    let head = repo.head_commit().context("resolving HEAD commit")?.id().detach();
    drop(repo);
    ingest_tips(repo_path, vec![head], cache_dir)
}

#[cfg(test)]
mod tests {
    use super::{classify_commit, parse_trailers, Level};

    #[test]
    fn commit_classification() {
        let c = |s: &str| classify_commit(s, false);
        // conventional with scope
        let (k, sc, conv, br) = c("feat(gitindex): dependencies layer");
        assert_eq!((k.as_str(), sc.as_str(), conv, br), ("feat", "gitindex", true, false));
        // conventional breaking
        let (k, _, conv, br) = c("feat!: drop old API");
        assert_eq!((k.as_str(), conv, br), ("feat", true, true));
        assert_eq!(c("fix: off-by-one").0, "fix");
        // not a known type → heuristic (a URL's colon must not fool it)
        assert_eq!(c("see http://x").3, false);
        assert_eq!(c("see http://x").2, false);
        // heuristic fallbacks
        assert_eq!(c("Add a new endpoint").0, "feat");
        assert_eq!(c("Fixed the crash").0, "fix");
        assert_eq!(c("Documentation for the API").0, "docs");
        assert_eq!(c("random subject").0, "other");
        // merges
        assert_eq!(classify_commit("Merge branch 'x'", true).0, "merge");
    }

    fn tr(msg: &str) -> Vec<(String, String)> {
        parse_trailers(msg.as_bytes())
    }

    #[test]
    fn trailers_basic() {
        let m = "Fix the thing\n\nSome body.\n\nCo-authored-by: Jane <j@x.com>\nSigned-off-by: Bob <b@y.com>\n";
        assert_eq!(
            tr(m),
            vec![
                ("Co-authored-by".into(), "Jane <j@x.com>".into()),
                ("Signed-off-by".into(), "Bob <b@y.com>".into()),
            ]
        );
    }

    #[test]
    fn trailers_subject_only_is_empty() {
        assert!(tr("Fix a bug").is_empty()); // no blank before → not a trailer block
    }

    #[test]
    fn trailers_prose_paragraph_is_not_trailers() {
        // a URL colon must not be read as a trailer key
        assert!(tr("Subject\n\nSee http://example.com for details.").is_empty());
        // a prose line in the last paragraph disqualifies the block
        assert!(tr("Subject\n\nThis fixes it.\nSigned-off-by: X").is_empty());
    }

    #[test]
    fn trailers_continuation_and_cherry_pick() {
        let m = "Subject\n\nBody.\n\nReviewed-by: Someone\n  With A Long Name <x@y.com>\n(cherry picked from commit abcdef)\n";
        assert_eq!(tr(m), vec![("Reviewed-by".into(), "Someone With A Long Name <x@y.com>".into())]);
    }

    #[test]
    fn level_ordering_and_suggest() {
        assert!(Level::Basic < Level::Mid && Level::Mid < Level::High);
        assert_eq!(Level::suggest(300_000, 10_000), Level::Basic); // huge by commits
        assert_eq!(Level::suggest(1_000, 50_000), Level::Basic);   // huge by files
        assert_eq!(Level::suggest(800, 1_700), Level::High);       // small (eos-like)
        assert_eq!(Level::suggest(50_000, 10_000), Level::Mid);    // medium
    }
}
