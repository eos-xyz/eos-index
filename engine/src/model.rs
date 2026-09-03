//! L1 core row model — the exact schema frozen in the  prototype
//! (`eng/index-prototype/schema.sql`). The Rust engine is a port of that
//! contract; the same git oracles check its output.

pub struct CommitRow {
    pub commit_sha: String,
    pub author_id: String,
    pub authored_at_epoch: i64,
    /// Author's timezone offset, minutes east of UTC (git stores it in the
    /// signature). `authored_at_epoch + offset*60` = the author's LOCAL wall clock
    /// — needed for "at what time of day does this person commit" ().
    pub authored_at_offset_minutes: i32,
    /// The COMMITTER (distinct from the author on a rebase/cherry-pick/merge), by
    /// email — references `authors` like `author_id` does (). The
    /// committer is also fed into the identity graph.
    pub committer_id: String,
    pub committed_at_epoch: i64,
    pub committed_at_offset_minutes: i32,
    pub subject: String,
    pub parent_count: i32,
    pub is_merge: bool,
    pub is_root: bool,
}

pub struct ParentRow {
    pub commit_sha: String,
    pub parent_index: i32,
    pub parent_sha: String,
}

pub struct AuthorRow {
    pub author_id: String,
    pub name: String,
    pub email: String,
    pub identity_id: i64, // resolved in assemble (); 0 as a placeholder
}

/// A resolved person: one or more author aliases (same person, different emails)
/// merged by union-find over git-derivable signals. `confidence` is the min over
/// the signals that formed the cluster (1.0 = sole alias).
pub struct IdentityRow {
    pub identity_id: i64,
    pub name: String,       // canonical display name
    pub email: String,      // canonical email
    pub confidence: f64,    // 1.0 = single alias; < 1 = heuristically merged
    pub alias_count: i32,
}

/// One author (email) and HOW it was linked to its identity — the alias
/// provenance the schema calls for, so a consumer can filter by method/confidence.
pub struct IdentityAliasRow {
    pub identity_id: i64,
    pub author_id: String, // normalized email (FK authors.author_id)
    pub name: String,
    pub email: String,
    pub method: String,    // sole | name-exact | forge-noreply | email-local
    pub confidence: f64,
}

/// A SUGGESTED merge from a weak signal (name similarity / shared email local),
/// NOT applied automatically — the review queue. Consumers/humans confirm before
/// merging, so weak signals never silently corrupt identity.
pub struct IdentityReviewRow {
    pub identity_a: i64,
    pub identity_b: i64,
    pub name_a: String,
    pub name_b: String,
    pub reason: String,   // name-similar | email-local
    pub similarity: f64,  // 0..1
}

pub struct FileRow {
    pub file_id: i64,
    pub path: String,
}

pub struct CommitFileRow {
    pub commit_sha: String,
    pub file_id: i64,
    pub old_path_id: Option<i64>,
    pub change_type: String,
    pub similarity: Option<i32>,
    pub added_lines: Option<i32>,
    pub removed_lines: Option<i32>,
    pub src_blob_sha: String,
    pub dst_blob_sha: String,
    /// git tree MODE of each side (): "100644" | "100755" | "120000" |
    /// "160000", "000000" if that side is absent. Captures chmod (exec-bit flip)
    /// and type-change events the numstat/blob columns can't express.
    pub src_mode: String,
    pub dst_mode: String,
}

/// Commit classification () — the KIND of each commit, inferred from its
/// subject: the Conventional-Commits type when the subject follows the spec
/// (`type(scope)!: …`), else a small keyword heuristic, and `merge` for merges.
/// Turns "how much of X kind of work" and "velocity by type" into one read.
/// Derived from `commits.subject`; the bench checks it equals the rule.
pub struct CommitClassRow {
    pub commit_sha: String,
    pub kind: String,           // feat|fix|docs|style|refactor|perf|test|build|ci|chore|revert|merge|other
    pub scope: String,          // conventional scope, e.g. feat(gitindex) → "gitindex"; else ""
    pub is_conventional: bool,  // subject matched the Conventional-Commits pattern
    pub is_breaking: bool,      // conventional "!" marker (feat!: …)
}

/// Per-commit size () — "how big is each commit and the diff between
/// it and its parent": files changed and lines added/removed, the rollup of
/// `commit_files`. Answers "how large are commits / commit-size over time" as one
/// read. Merges carry no diff, so they don't appear (commit_files excludes them).
/// Derived; the bench checks it equals the rollup of the (git-verified)
/// commit_files.
pub struct CommitStatRow {
    pub commit_sha: String,
    pub files_changed: i32,
    pub insertions: i64, // Σ added_lines (binary counts 0, like git)
    pub deletions: i64,  // Σ removed_lines
    pub net_lines: i64,  // insertions - deletions
}

/// One line of a current (HEAD) file and the commit that last touched it.
pub struct BlameLineRow {
    pub file_id: i64,
    pub line_number: i32,
    pub commit_sha: String,
}

/// Ownership at a historical snapshot () — repo-wide owned lines per
/// person, per snapshot (tag/release), identity-resolved. Turns ownership and bus
/// factor into a TIME SERIES: "who owned how much of the codebase at v1 vs v2".
/// A rollup of `blame_snapshots` (so empty unless `EOS_SNAPSHOTS` is set). Derived;
/// the bench checks it equals the definitional query.
pub struct SnapshotOwnershipRow {
    pub snapshot_ref: String,  // the tag/rev requested
    pub snapshot_sha: String,  // the commit it resolved to
    pub identity_id: i64,
    pub owned_lines: i64,      // lines this person owns across the whole snapshot
    pub total_lines: i64,      // total blamed lines in the snapshot (the denominator)
    pub ownership_share: f64,  // owned_lines / total_lines, in (0,1]
}

/// One line of a file as it existed at a historical SNAPSHOT (a tag/rev), and the
/// commit that last touched it as of that snapshot. Path-keyed, not file_id-keyed:
/// a file present at an old snapshot may have been deleted or renamed before HEAD,
/// so it has no HEAD file id. Populated only when `EOS_SNAPSHOTS` requests it
/// (.7b — historical provenance); the table is empty otherwise.
pub struct SnapshotBlameRow {
    pub snapshot_ref: String, // the ref/rev requested (tag name or user rev string)
    pub snapshot_sha: String, // the commit it resolved to (40-hex)
    pub path: String,
    pub line_number: i32,     // 1-based, in the snapshot version of the file
    pub commit_sha: String,   // origin commit that last changed the line, as of the snapshot
}

/// Materialized ownership (.10b): for each HEAD file, how many of its
/// current lines each resolved person owns (git blame, identity-resolved) and that
/// person's share. Precomputed so ownership is a one-line query — no
/// blame→commits→authors→identities join — and identity-resolved so a person's
/// several emails don't fragment the count. A file's dominant owner is its
/// `max(ownership_share)` row. Derived, not a git primitive: the bench checks it
/// equals the definitional query over blame + commits + authors.
pub struct FileOwnershipRow {
    pub file_id: i64,
    pub identity_id: i64,
    pub owned_lines: i64,
    pub file_lines: i64,      // total blamed lines in the file (the share denominator)
    pub ownership_share: f64, // owned_lines / file_lines, in (0,1]
}

/// Temporal coupling (, mid+): a pair of files that change together in
/// the same commit `co_changes` times — a hidden dependency even without an import
/// edge. One row per unordered pair (`file_a_id < file_b_id`). Bulk commits (a mass
/// move/reformat touching many files) are excluded so they don't couple everything
/// to everything, and a pair must co-change a minimum number of times to appear.
/// Derived, not a git primitive — the bench checks it equals its definitional query
/// over `commit_files`.
pub struct CouplingRow {
    pub file_a_id: i64,
    pub file_b_id: i64,
    pub co_changes: i64,
}

/// Collaboration graph (, mid+): how much two people work together,
/// inferred from the code alone — a pair collaborates as much as they edit the
/// SAME files. One row per unordered person pair (`identity_a < identity_b`), with
/// a Jaccard `strength`. Hub files (touched by many people — lockfiles, globals),
/// bulk commits, and bot authors are excluded so they don't couple everyone to
/// everyone. Derived; the bench checks it equals its definitional query.
pub struct CollaborationRow {
    pub identity_a: i64,
    pub identity_b: i64,
    pub shared_files: i64, // distinct files both edited (non-hub, non-bulk, non-bot)
    pub a_files: i64,      // distinct files A edited (same filtering)
    pub b_files: i64,
    pub strength: f64,     // Jaccard: shared / (a_files + b_files - shared)
}

/// A code-intelligence finding (, mid+) — the "briefing" layer that
/// turns the composite indices into readable, typed insights instead of raw rows.
/// Each row is one finding with a severity and a human sentence. Heuristic (no git
/// oracle), but each `kind` is DEFINITIONAL: its rows are exactly the rule's query
/// over the composite tables, so the bench can still check it.
pub struct InsightRow {
    pub kind: String,     // bus_factor_risk | area_key_person | hotspot | hidden_coupling
    pub severity: String, // critical | warning | info
    pub subject: String,  // what it's about — a path, an area, or "a ↔ b"
    pub metric: f64,      // the driving number (share, churn count, co-changes)
    pub detail: String,   // one-line human-readable finding
}

/// Area ownership (, mid+) — the person×area composite: HEAD ownership
/// rolled up from files to their directory ("area"), identity-resolved. The unit
/// of "who knows this module": for each (area, person), how many of the area's
/// current lines they own and their share. One canonical area definition (the
/// file's immediate parent directory, "." for repo root) so every consumer rolls
/// up the same way. Derived; the bench checks it equals the rollup of
/// `file_ownership`.
pub struct AreaOwnershipRow {
    pub area: String,         // immediate parent directory of the files; "." for root
    pub identity_id: i64,
    pub owned_lines: i64,     // HEAD blame lines this person owns across the area
    pub area_lines: i64,      // total blamed lines in the area (the share denominator)
    pub ownership_share: f64, // owned_lines / area_lines, in (0,1]
}

/// A potential leaked secret () — a known credential SHAPE found in a
/// HEAD text file. Defensive-security signal. Stores the rule, the line, and a
/// MASKED preview (a type prefix + `…`) — never the secret value.
pub struct SecretFindingRow {
    pub file_id: i64,
    pub line: i32,
    pub rule: String,    // aws_access_key | github_token | google_api_key | slack_token | stripe_secret_key | private_key
    pub preview: String, // masked: type prefix + "…"
}

/// A test file () — a HEAD file identified as tests by its path/name.
/// The base of "is this module tested / how much": test-file density per area now,
/// and (with coupling) heuristic coverage later. Path-based and language-aware, so
/// it's cheap and deterministic; the bench checks it equals the rule.
pub struct TestFileRow {
    pub file_id: i64,
    pub lang: String,   // ts|js|py|go|rust|java|rb|… (from the extension)
    pub signal: String, // the rule that matched, e.g. "*.test.*", "dir:__tests__", "*_test.go"
}

/// A generated or vendored HEAD file () — code not authored by hand in
/// this repo: build output, tool-emitted code, checked-in dependencies. The base
/// of "count only human-authored code": exclude these from ownership, churn and
/// hotspot signals so a 40k-line lockfile isn't bus-factor noise. Path-based and
/// deterministic (the low-false-positive subset of GitHub Linguist's rules), so
/// it's cheap and always on; the bench checks it equals the rule over HEAD paths.
pub struct GeneratedFileRow {
    pub file_id: i64,
    pub category: String, // generated | vendored
    pub reason: String,   // the rule that matched, e.g. "lockfile", "minified", "vendored-dir"
}

/// Test→source coverage () — the source file a test most likely covers,
/// matched by name (its target stem → a non-test source of the same language, a
/// same-directory or repo-unique match). Heuristic; the bench checks it equals the
/// rule. Turns "is this file tested" into a join, and feeds coverage-per-area.
pub struct TestCoverageRow {
    pub test_file_id: i64,
    pub source_file_id: i64,
    pub method: String, // same_dir | unique_stem
}

/// A technical-debt marker () — a `TODO`/`FIXME`/`HACK`/`XXX`/`NOTE`
/// left in a HEAD text file. One row per occurrence (file, line, marker, the text
/// after it). The debt-density signal: "which modules carry the most unfinished
/// work". Content signal, mid+ tier; the bench checks each marker is really at
/// that line of the HEAD blob.
pub struct CodeMarkerRow {
    pub file_id: i64,
    pub line: i32,       // 1-based, in the HEAD version of the file
    pub marker: String,  // TODO | FIXME | HACK | XXX | NOTE
    pub text: String,    // the rest of the line after the marker (trimmed, truncated)
}

/// Content-addressed per-blob facts () — one row per DISTINCT HEAD blob,
/// keyed by its git blob SHA (the content address). Every column is a pure function
/// of the blob's bytes (size, line count, binary-ness), so the same content in any
/// path, commit, or tenant yields the same row. This is the cacheable/shareable
/// unit the cross-tenant moat is built on: extract once per unique blob, reuse
/// everywhere the identical blob appears (~45% cross-tenant overlap per spikes 3+4;
/// within a repo it dedups blob instances across paths and history). mid+ tier.
pub struct BlobFactRow {
    pub blob_sha: String,   // git blob object id (40-hex) — the content address
    pub size_bytes: i64,    // exact blob size (== git cat-file -s)
    pub line_count: i64,    // git's line count (\n count + a trailing partial line)
    pub is_binary: bool,    // NUL in the first 8000 bytes (git's own heuristic)
}

/// One definition (L3 symbols) in a HEAD file, keyed by the file's blob SHA
/// (the content address). start/end lines are 1-based in the HEAD version.
pub struct SymbolRow {
    pub file_id: i64,
    pub blob_sha: String,
    pub name: String,
    pub kind: String, // function|method|class|interface|type|enum|struct|trait|const|…
    pub start_line: i32,
    pub end_line: i32,
    pub lang: String, // ts|tsx|rust|python
}

/// One lexical reference (L3) — a usage site (call/new/macro) of a repo-defined
/// name. Name-based, not a resolved binding; join to `symbols` on `name`.
pub struct SymbolRefRow {
    pub file_id: i64,
    pub blob_sha: String,
    pub name: String,
    pub ref_kind: String, // call (bare foo()) | method (x.foo()) | new | macro
    pub line: i32,
    pub lang: String,
    pub def_file_id: Option<i64>, // resolved target file (import/local), else NULL
}

/// One edge of the symbol reference graph (GRAIL, ): a usage site,
/// attributed to the definition that ENCLOSES it (the caller `src_*`) and pointing
/// at the referenced name (`dst_*`). Built from `symbols` + `symbol_refs` — a
/// symbol-level call/reference graph for impact analysis ("what calls X"),
/// dependency ("what does Y use"), and dead-symbol candidates (no inbound edges).
/// Lexical, not semantic: the callee is name-based, resolved to a file via imports
/// where possible (`dst_file_id`) and to a single symbol only when that (file,name)
/// is unambiguous (`dst_start_line` set). `ref_kind` is carried so consumers can
/// gate on the high-precision kinds (`call`/`new`) vs noisier `method`.
pub struct SymbolEdgeRow {
    pub src_file_id: i64,
    pub src_name: String,
    pub src_kind: String,
    pub src_start_line: i32, // identifies the caller symbol (file_id,name,start_line)
    pub dst_name: String,
    pub dst_file_id: Option<i64>,    // resolved target file (symbol_refs.def_file_id), else NULL
    pub dst_start_line: Option<i32>, // set iff (dst_file_id,dst_name) is a single symbol — fully resolved
    pub ref_kind: String,            // call | method | new | macro
    pub line: i32,                   // call-site line, in the src file
    pub lang: String,
}

/// A file change introduced by a MERGE commit relative to its FIRST parent
/// (). `commit_files` covers only non-merge commits (so churn/coupling
/// aren't inflated by merges); this is the parallel table for merges, so "what did
/// this merge bring onto mainline" is queryable and first-parent history is
/// complete. Path-keyed (historical paths don't all map to HEAD file_ids). The
/// bench checks the changed-path set against `git diff-tree <merge>^1 <merge>`.
pub struct MergeChangeRow {
    pub commit_sha: String,
    pub change_type: String,      // A | M | D | R | T
    pub path: String,             // the (new) path
    pub old_path: Option<String>, // Some for a rename — the previous path
    pub similarity: Option<i32>,
    pub added_lines: Option<i32>,   // None for binary
    pub removed_lines: Option<i32>,
    pub src_blob_sha: String,
    pub dst_blob_sha: String,
    pub src_mode: String,
    pub dst_mode: String,
}

/// Each commit's ROOT tree () — the entry point into `tree_objects`, so
/// "the file tree at commit X" is a lookup + recursive expansion. Opt-in with the
/// historical-tree layer (EOS_TREES / high).
pub struct CommitTreeRow {
    pub commit_sha: String,
    pub root_tree_sha: String,
}

/// One direct entry of a DISTINCT git tree object () — the historical
/// directory structure, content-addressed by `tree_sha` so unchanged subtrees are
/// stored once across all of history (git's own dedup; bounded). Recursing from a
/// commit's root (see `commit_trees`) reconstructs the full file list at ANY commit
/// without the git object store — the lossless historical-tree layer.
pub struct TreeObjectRow {
    pub tree_sha: String,    // the containing tree's object id
    pub seq: i32,            // entry order within the tree
    pub name: String,        // entry name (one path segment)
    pub mode: String,        // git mode (100644, 100755, 120000, 040000, 160000)
    pub entry_type: String,  // blob | executable | symlink | tree | submodule
    pub entry_sha: String,   // the child object id (a blob, subtree, or gitlink commit)
}

/// A distinct content chunk (FastCDC, ) — the deduplicated store. Keyed
/// by a 128-bit content hash of the chunk bytes, so identical content across
/// blobs (and, in principle, across tenants) is one row. `ref_count` is how many
/// blob positions reference it. `bytes` holds the chunk's actual content, so the
/// store is LOSSLESS: any blob is `concat(bytes)` over its `blob_chunks` in `seq`
/// order — the index can reconstruct file content without the git object store.
/// Populated only when `EOS_CHUNK` requests it.
pub struct ChunkRow {
    pub chunk_hash: String, // xxh3-128 of the chunk bytes, hex
    pub bytes: Vec<u8>,     // the chunk's raw content (stored once per unique chunk)
    pub size: i32,
    pub ref_count: i32,
}

/// One chunk occurrence inside a blob (FastCDC): the ordered membership that lets
/// a blob be reconstructed as the concatenation of its chunks. Path-independent —
/// keyed by blob SHA (content address), so a blob at many paths is chunked once.
pub struct BlobChunkRow {
    pub blob_sha: String,
    pub seq: i32,       // 0-based order within the blob
    pub offset: i64,    // byte offset of the chunk in the blob
    pub size: i32,
    pub chunk_hash: String,
}

/// One parsed trailer of a commit message () — a `Key: value` line in
/// the message's trailer block (`Co-authored-by`, `Signed-off-by`, `Reviewed-by`,
/// …). Git-native review/collaboration signal that the author field alone misses.
pub struct TrailerRow {
    pub commit_sha: String,
    pub seq: i32, // order within the commit's trailer block, 0-based
    pub key: String,
    pub value: String,
}

/// The full message + encoding + signature presence of a commit ().
/// Separate from `commits` so the (potentially long) body doesn't bloat the
/// scannable commit table. `subject` stays in `commits`.
pub struct MessageRow {
    pub commit_sha: String,
    pub body: Option<String>,     // message minus the subject line; None if empty
    pub encoding: Option<String>, // non-UTF-8 encoding header, if any
    pub is_signed: bool,          // has a `gpgsig` header (presence, not validity)
}

/// One diff hunk of a commit () — a contiguous changed region of a file,
/// `@@ -old_start,old_lines +new_start,new_lines @@`. Computed at zero context, so
/// `sum(new_lines)` per file = added lines and `sum(old_lines)` = removed lines.
/// The real patch structure the numstat counts summarize.
pub struct HunkRow {
    pub commit_sha: String,
    pub file_id: i64,
    pub seq: i32,
    pub old_start: i32,
    pub old_lines: i32,
    pub new_start: i32,
    pub new_lines: i32,
}

/// One entry of the HEAD tree () — a file with its git MODE, capturing
/// permissions/executable-bit, symlinks and submodules the path/blob tables miss.
pub struct TreeEntryRow {
    pub file_id: i64,
    pub path: String,
    pub mode: String,        // octal git mode: 100644 | 100755 | 120000 | 160000
    pub entry_type: String,  // blob | executable | symlink | submodule
    pub blob_sha: String,    // HEAD blob (or the pointed commit for a submodule)
    pub size: Option<i64>,   // bytes; None for submodules
}

/// A submodule () — a `.gitmodules` declaration joined to its HEAD-tree
/// gitlink pin. `url` (where it points in the world) lives only in `.gitmodules`;
/// `pinned_sha` (the exact commit the superproject pins) lives only in the tree.
/// The two presence flags are kept because they can disagree.
pub struct SubmoduleRow {
    pub path: String,              // submodule path in the superproject (the join key)
    pub name: Option<String>,      // `[submodule "<name>"]` section name; None if only a gitlink
    pub url: Option<String>,       // .gitmodules url; None if not declared
    pub branch: Option<String>,    // .gitmodules branch, if set
    pub pinned_sha: Option<String>, // the 160000 gitlink commit; None if declared but absent from HEAD
    pub in_gitmodules: bool,
    pub in_tree: bool,
}

/// One internal module dependency () — file A depends on file B when a
/// reference in A resolves to a definition in B. The architecture graph *inside*
/// the repo (the counterpart to `dependencies`, which is the external side): a
/// rollup of `symbol_refs` by (file, resolved-target-file), self-edges excluded.
/// "What does this file use / what uses it", and (with hotspots) the coupling that
/// matters. Derived; the bench checks it equals the rollup of symbol_refs.
pub struct ModuleDepRow {
    pub from_file_id: i64,
    pub to_file_id: i64,
    pub ref_count: i64, // references in `from` resolved to a definition in `to`
}

/// One declared dependency () — a package a repo depends on, parsed
/// from a HEAD manifest (package.json, Cargo.toml, …). The external side of the
/// architecture graph: what the code relies on, per manifest, with the version
/// spec as written and the dependency scope. HEAD-derived; git is the oracle (it's
/// exactly what the tracked manifest declares). Rust engine only.
pub struct DependencyRow {
    pub manifest_path: String, // the HEAD manifest that declares it
    pub ecosystem: String,     // npm | cargo | (pypi | go | maven | rubygems — later)
    pub name: String,          // package name
    pub version: String,       // version spec/constraint AS DECLARED (raw), "" / "workspace" when none
    pub scope: String,         // runtime | dev | build | peer | optional
}

/// A git reference — branch, tag (lightweight or annotated), remote-tracking
/// branch, or the symbolic HEAD (). The ref topology as data.
pub struct RefRow {
    pub name: String,                     // full refname, e.g. refs/heads/main, refs/tags/v1, HEAD
    pub kind: String,                     // branch | remote-branch | tag-lightweight | tag-annotated | symbolic | other
    pub object_sha: String,               // what the ref points at directly
    pub peeled_commit_sha: Option<String>, // the commit it resolves to (deref annotated tags); None if it isn't a commit
    pub is_symbolic: bool,
    pub tagger_name: Option<String>,      // annotated tags only
    pub tagger_email: Option<String>,
    pub tagged_at_epoch: Option<i64>,
    pub tag_subject: Option<String>,
    pub tag_body: Option<String>,         // annotated tag message body (below the subject)
}

/// A git note () — a `refs/notes/*` annotation attached to an object
/// (usually a commit), stored out-of-band from the object itself. git's one object
/// class the index didn't capture; here so `high` is lossless. The note's text is
/// the content of a blob keyed by (notes ref, target).
pub struct NoteRow {
    pub notes_ref: String,   // the notes ref, e.g. refs/notes/commits
    pub target_sha: String,  // the annotated object (commit/blob/…)
    pub note_sha: String,    // the note blob's object id
    pub body: String,        // the note text
}

/// Everything one ingest pass produces.
pub struct Ingested {
    pub commits: Vec<CommitRow>,
    pub refs: Vec<RefRow>,
    pub notes: Vec<NoteRow>,
    pub submodules: Vec<SubmoduleRow>,
    pub dependencies: Vec<DependencyRow>,
    pub code_markers: Vec<CodeMarkerRow>,
    pub secret_findings: Vec<SecretFindingRow>,
    pub test_files: Vec<TestFileRow>,
    pub test_coverage: Vec<TestCoverageRow>,
    pub generated_files: Vec<GeneratedFileRow>,
    pub blob_facts: Vec<BlobFactRow>,
    pub tree_entries: Vec<TreeEntryRow>,
    pub commit_messages: Vec<MessageRow>,
    pub commit_trailers: Vec<TrailerRow>,
    pub parents: Vec<ParentRow>,
    pub authors: Vec<AuthorRow>,
    pub files: Vec<FileRow>,
    pub commit_files: Vec<CommitFileRow>,
    pub merge_changes: Vec<MergeChangeRow>,
    pub commit_stats: Vec<CommitStatRow>,
    pub commit_classes: Vec<CommitClassRow>,
    pub hunks: Vec<HunkRow>,
    pub coupling: Vec<CouplingRow>,
    pub collaboration: Vec<CollaborationRow>,
    pub area_ownership: Vec<AreaOwnershipRow>,
    pub insights: Vec<InsightRow>,
    pub blame: Vec<BlameLineRow>,
    pub blame_snapshots: Vec<SnapshotBlameRow>,
    pub snapshot_ownership: Vec<SnapshotOwnershipRow>,
    pub file_ownership: Vec<FileOwnershipRow>,
    pub identities: Vec<IdentityRow>,
    pub identity_aliases: Vec<IdentityAliasRow>,
    pub identity_reviews: Vec<IdentityReviewRow>,
    pub symbols: Vec<SymbolRow>,
    pub symbol_refs: Vec<SymbolRefRow>,
    pub module_deps: Vec<ModuleDepRow>,
    pub symbol_edges: Vec<SymbolEdgeRow>,
    pub chunks: Vec<ChunkRow>,
    pub blob_chunks: Vec<BlobChunkRow>,
    pub commit_trees: Vec<CommitTreeRow>,
    pub tree_objects: Vec<TreeObjectRow>,
}
