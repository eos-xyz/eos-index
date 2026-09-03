# gitindex

**Turn any git repository into a fresh, complete, queryable SQL database.**
`gitindex` reads a repo's object store and materialises it as Parquet —
commits, diffs, renames, blame, ancestry — **verified line-for-line against
git**. Query it with DuckDB, Postgres, pandas, or anything that speaks SQL. No
SDK, no API: SQL is the interface.

## Benchmark — verified against git

git is the oracle. Every table is checked against git's own commands — and where
a number is diff-algorithm-dependent we use **git's actual xdiff (libgit2)** so
it matches bit-for-bit. Reproduce with `cd ../bench && npm i && node
--experimental-strip-types src/bench.ts run gitindex`.

| we compute | checked against | result |
|---|---|---|
| commit count | `git rev-list --count HEAD` | **exact** |
| changed-file set (A/M/D/T/R) | `git show --name-status` | **exact** |
| added/removed line counts | `git --numstat` | **bit-exact** |
| renames (exact + inexact) | `git -M` / `git log --follow` | **superset** — finds every rename git does (`miss=0`) plus more (`rename_recall_delta > 0`) |
| ancestry | `git merge-base --is-ancestor` | **exact** |
| blame (line → origin commit) | `git blame` | **100%** (default); `EOS_BLAME=native` experimental (99.7% small, 95% at scale — not recommended) |

Fast, and incremental — `gitindex` records the indexed HEAD, so the next index
processes **only the delta** (new commits + re-blames of only changed files).
"Reindex" disappears. Measured on eos-monorepo (~800 commits, 1.6k files):

| operation | wall-clock | note |
|---|---|---|
| full index | **~14 s** | cold, whole repo (mid tier) |
| push-sized update | **~3 s** | `incremental_cost_ratio ≈ 0.20`; single-commit push is cheaper |

The full index is blame-bound, and about half the HEAD files were added in a
single commit — a trivial blame we now compute without `git blame` (read the blob,
count lines), roughly halving a cold index (basic tier ~20 s → ~12 s). Bit-exact
vs `git blame` still (`blame_sample` = 100%).

The incremental output is byte-equivalent to a full reindex (`bench incremental`
proves it).

At scale: **vscode** (v1.85.0, **116,608 commits**, 30k files) indexes on a laptop
in **~16 min / ~1 GB RAM / 105 MB**, **all oracles green** (commit count exact,
numstat bit-exact, blame 100%). Full table + method: [`../BENCHMARKS.md`](../BENCHMARKS.md).

## How it compares

Most tools near this space solve a *different* problem — an editor plugin, a
PR-workflow app, an LLM-prompt flattener, a code-search engine. `gitindex` is the
one that turns a repo into a **headless, git-verified, queryable index** of its
whole history *and* code structure. This compares **capabilities, not products**;
several rows are a different category on purpose.

| | whole-history index | git-verified output | headless & local | history: blame + renames | code graph: symbols/refs/calls | you query it as… |
|---|:--:|:--:|:--:|:--:|:--:|---|
| **EOS `gitindex`** | ✅ full history | ✅ line-for-line vs git | ✅ CLI, private | ✅ | ✅ 8 languages | **Parquet / SQL** |
| GitLens *(GitKraken)* | ✗ live view, not indexed | shows live git (unverified) | ✗ in-editor (local) | ✅ interactive | ✗ | you *read* it in VS Code |
| Graphite | ✗ | — | ✗ SaaS + CLI | ✗ | ✗ | a review / stacked-PR app |
| git-ingest | ✗ HEAD only | ✗ | ✅ local | ✗ | ✗ | a text digest for an LLM |
| Sourcegraph / SCIP | partial | ✗ (nav, not git-diff) | self-hostable | partial | ✅ SCIP indexers | code search + navigation |
| `scc` / `tokei` | ✗ | ✗ | ✅ local | ✗ | ✗ | line-count stats |

**The distinction, per tool:**

- **GitLens** — the best-in-class *in-editor* git experience: inline blame, file
  history (following renames), a commit graph, all live inside VS Code. It renders
  git for a human, one file at a time; it does not emit a headless, queryable index
  of the whole history you can run analytics or SQL over — the file you open, not
  the repo as data.
- **Graphite** — a code-review / stacked-PR workflow product (SaaS + CLI). It
  operates *on* your PRs; it isn't a history index at all. Adjacent, not competing.
- **git-ingest** — flattens the current HEAD into one text blob to paste into an
  LLM. Local and handy, but HEAD-text only: no history, no blame, no renames, no
  symbol graph, nothing to query.
- **Sourcegraph / SCIP** — the closest on *code navigation*: SCIP indexers give
  precise cross-repo symbol/reference/definition graphs, self-hostable. It's built
  for code search and go-to-definition, not for git-diff-verified history analytics
  (blame/renames/ownership as queryable tables); its focus is the code graph, EOS's
  is the code graph **plus** the whole git history, both checked against git.
- **`scc` / `tokei`** — fast line/complexity counters. No history, no graph.

**Honesty first:** the cells for other tools are read from their **public docs and
generally-known behaviour, not our own hands-on measurement** — verify before
quoting in anything customer-facing. "Different category" is not a knock: GitLens
is an excellent editor experience and git-ingest is a neat LLM-prep tool. The point
is that none of them is a *complete, fresh, git-verified index of the whole history
and code structure that you query with plain SQL* — which is what EOS is.

## Install

**Prebuilt binary** (no Rust toolchain) — download the archive for your platform
from the releases, unpack, and put `gitindex` on your `PATH`:

```bash
# macOS (arm64 / amd64), Linux (amd64 / arm64), Windows (amd64)
tar xzf gitindex-<platform>.tar.gz && sudo mv gitindex /usr/local/bin/
gitindex --version
```

Releases are built by [`.github/workflows/release-gitindex.yml`](../../.github/workflows/release-gitindex.yml)
on a `gitindex-v*` tag; to produce them locally run
[`scripts/build-release.sh`](scripts/build-release.sh) (macOS arches natively,
Linux via Docker).

**From source** (needs a Rust toolchain):

```bash
cargo build --release   # → ./target/release/gitindex
```

`git` must be on `PATH` at runtime (used for the one order-dependent computation,
blame). Apache-2.0.

## Quickstart

```bash
gitindex index /path/to/repo --out ./index
duckdb -c "SELECT * FROM './index/commits.parquet' LIMIT 5"
```

## The tables (L1 core schema)

See [`../index-prototype/schema.sql`](../index-prototype/schema.sql).

| table | one row per | notable columns |
|---|---|---|
| `commits` | commit reachable from HEAD | `author_id` + **`committer_id`** (distinct on rebase/merge), `authored_at`/`committed_at` (+ tz offsets), `subject`, `is_merge`, `is_root` |
| `refs` | branch / tag / remote / HEAD | `kind`, `object_sha`, `peeled_commit_sha` (annotated tags), tagger info — the ref topology |
| `commit_messages` | commit body/encoding/signature | `body`, `encoding`, `is_signed` (has a gpgsig header) |
| `commit_trailers` | trailer of a commit message | `key`/`value` — Co-authored-by, Signed-off-by, Reviewed-by (review/pairing signal, no forge API) |
| `commit_parents` | parent edge | `parent_index` (0 = first parent) |
| `commit_files` | file changed by a commit | **`old_path_id`** (rename), `change_type`, `added/removed_lines`, blob SHAs |
| `files` | distinct path ever seen | `file_id`, `path` |
| `tree_entries` | HEAD file with its mode | `mode`, `entry_type` (blob/executable/symlink/submodule), `blob_sha`, `size` |
| `blame` | line of a HEAD file | `line_number`, `commit_sha` (origin) |
| `blame_snapshots` | line of a file at a past snapshot | `snapshot_ref`/`snapshot_sha`, `path`, `line_number`, `commit_sha` — opt-in (`EOS_SNAPSHOTS`) |
| `file_ownership` | HEAD file × person | `owned_lines`, `file_lines`, `ownership_share` — materialized, identity-resolved |
| `authors` | commit author (by email) | `identity_id` |
| `identities` | resolved person (union-find) | canonical name/email, `confidence`, `alias_count` |
| `identity_aliases` | author→identity link | `method` (sole/name-exact/forge-noreply/…), `confidence` |
| `identity_reviews` | suggested merge (review queue) | `identity_a/b`, `reason`, `similarity` — not auto-applied |
| `symbols` | definition in a HEAD file (L3) | **`blob_sha`** (content address), `name`, `kind`, `start/end_line`, `lang` |
| `symbol_refs` | usage site of a repo-defined name (L3) | **`blob_sha`**, `name`, `ref_kind` (call/method/new/macro), `line`, `lang`, **`def_file_id`** (resolved target) |
| `symbol_edges` | symbol→symbol graph edge (GRAIL) | `src_*` (enclosing caller), `dst_*` (callee, `dst_start_line` set when pinned), `ref_kind`, `line` |
| `chunks` | distinct content chunk (FastCDC) | `chunk_hash` (xxh3-128), `size`, `ref_count` — the deduped store; opt-in (`EOS_CHUNK`) |
| `blob_chunks` | chunk occurrence in a blob | `blob_sha`, `seq`, `offset`, `size`, `chunk_hash` — a blob = its chunks concatenated |

## Query it

```bash
# commits per author
duckdb -c "SELECT a.name, count(*) c FROM './index/commits.parquet' cm
           JOIN './index/authors.parquet' a USING(author_id)
           GROUP BY 1 ORDER BY c DESC LIMIT 10"

# who last touched each line of a file (blame)
duckdb -c "SELECT b.line_number, c.subject FROM './index/blame.parquet' b
           JOIN './index/files.parquet' f USING(file_id)
           JOIN './index/commits.parquet' c ON b.commit_sha = c.commit_sha
           WHERE f.path = 'README.md' ORDER BY b.line_number"

# most-renamed files
duckdb -c "SELECT f.path, count(*) n FROM './index/commit_files.parquet' cf
           JOIN './index/files.parquet' f USING(file_id)
           WHERE cf.change_type='R' GROUP BY 1 ORDER BY n DESC LIMIT 10"

# who owns each file (dominant owner by blame lines) — materialized, no joins to
# blame/commits/authors needed; identity-resolved
duckdb -c "SELECT f.path, i.name AS owner, o.owned_lines, round(o.ownership_share,2) AS share
           FROM './index/file_ownership.parquet' o
           JOIN './index/files.parquet' f USING(file_id)
           JOIN './index/identities.parquet' i USING(identity_id)
           QUALIFY row_number() OVER (PARTITION BY f.path ORDER BY o.owned_lines DESC)=1
           ORDER BY o.owned_lines DESC LIMIT 10"

# bus factor: files a single person owns > 90% of (knowledge concentration risk)
duckdb -c "SELECT f.path, i.name AS owner, round(o.ownership_share,2) AS share
           FROM './index/file_ownership.parquet' o
           JOIN './index/files.parquet' f USING(file_id)
           JOIN './index/identities.parquet' i USING(identity_id)
           WHERE o.ownership_share > 0.9 ORDER BY o.file_lines DESC LIMIT 10"

# every function/class/type defined in a file (L3 symbols)
duckdb -c "SELECT s.name, s.kind, s.start_line FROM './index/symbols.parquet' s
           JOIN './index/files.parquet' f USING(file_id)
           WHERE f.path = 'src/server.ts' ORDER BY s.start_line"

# find-usages resolved across imports: call sites that bind to a specific file
duckdb -c "SELECT rf.path AS used_in, r.line, df.path AS defined_in
           FROM './index/symbol_refs.parquet' r
           JOIN './index/files.parquet' rf ON r.file_id = rf.file_id
           JOIN './index/files.parquet' df ON r.def_file_id = df.file_id
           WHERE r.name = 'computeBlame' ORDER BY 1,2"

# call graph (GRAIL): which symbols call a given function, and from where
duckdb -c "SELECT sf.path AS caller_file, e.src_name AS caller, e.line
           FROM './index/symbol_edges.parquet' e
           JOIN './index/files.parquet' sf ON e.src_file_id = sf.file_id
           WHERE e.dst_name = 'computeBlame' AND e.ref_kind IN ('call','new')
           ORDER BY 1,3"

# dead-symbol candidates: functions/methods no resolved edge points at
duckdb -c "SELECT f.path, s.name, s.kind FROM './index/symbols.parquet' s
           JOIN './index/files.parquet' f USING(file_id)
           WHERE s.kind IN ('function','method')
             AND NOT EXISTS (SELECT 1 FROM './index/symbol_edges.parquet' e
                             WHERE e.dst_file_id = s.file_id AND e.dst_name = s.name)
           LIMIT 20"

# content dedup savings (needs EOS_CHUNK): stored bytes vs unique chunk bytes
duckdb -c "SELECT (SELECT sum(size) FROM './index/blob_chunks.parquet') AS stored,
                  (SELECT sum(size) FROM './index/chunks.parquet')      AS unique_bytes,
                  round((SELECT sum(size) FROM './index/blob_chunks.parquet')::double
                      / (SELECT sum(size) FROM './index/chunks.parquet'), 2) AS dedup_x"
```

## Usage

```
gitindex index <repo> [--out ./index] [--full] [--refs head|active|all] [--cache <dir>]
```

- Incremental by default; `--full` forces a full reindex.
- **`EOS_INDEX_LEVEL=basic|mid|high`** (default `mid`) — how much to precompute, the
  index-time ↔ access-speed ↔ memory trade-off:
  - **basic** — L1 only: commits, parents, files, changes, authors, identities,
    blame. Fastest, least memory.
  - **mid** *(default)* — + materialized `file_ownership` and L3 `symbols` /
    `symbol_refs`. The balanced tier.
  - **high** — + the `symbol_edges` call graph (GRAIL), and — unless their env
    overrides — historical blame snapshots (`EOS_SNAPSHOTS=tags`), content dedup
    (`EOS_CHUNK=history`), and **all active branches** (`--refs auto` resolves to
    `active` at `high`: the default branch + every branch with activity in the last
    90 days; only genuinely-abandoned branches, stale beyond the window, are deferred
    as cheap cache-fills). Maximum access; most time and memory.

  On eos-monorepo: basic ~12 s, mid ~14 s, high ~75 s (high re-blames every tag).
  The bench stays green at every level — absent tiers report `NOT_IMPLEMENTED`, not
  a failure. `EOS_SNAPSHOTS` / `EOS_CHUNK` still work as fine-grained overrides on
  top of any level. The engine **suggests** (never silently changes) a level from
  repo size: a huge repo run at the default prints a `basic` hint, and `high` on a
  non-trivial repo prints a cost note — advisory only, your choice always wins.
- `--refs` (): `head` (default branch, the hot path), `active`
  (default + branches active within 90 days), or `all`. Deferred (abandoned)
  branches are logged, not dropped — a later branch is a cheap cache-fill.
- `--cache <dir>` (): a **content-addressed** blob-fact cache keyed by
  blob SHA. Share one dir across repos/tenants and a blob is profiled once, ever
  (the dedup "moat"); the engine reports `dedup_hit_rate`. Only derived facts are
  cached, never blob contents (see `../hosted/DATA-BOUNDARY.md`).

---

## How it works (engineering notes)

Same L1 schema as the  TypeScript prototype ([`../index-prototype`](../index-prototype));
this is the Rust engine, checked against the same git oracles via [`../bench`](../bench).

- `src/ingest.rs` — walk the commit graph (`gix`); `walk` gathers path-keyed raw
  parts over a commit range, `assemble` turns them into the id-keyed tables
  (shared by full and incremental).
- `src/diff.rs` — tree diff with **subtree-hash pruning** (skip a subtree whose
  OID is unchanged → O(changed·depth)); line counts via git's xdiff (libgit2).
- `src/rename.rs` — exact renames (blob-SHA hash join) + inexact renames
  (line-MinHash + LSH candidates, **no 1000-file cap** — the differentiator).
- `src/blame.rs` — `git blame` per HEAD file, parallel (rayon); `EOS_BLAME=native`
  switches to in-process gitoxide blame (`gix::blame`).
- `src/symbols.rs` — L3 definitions **and** references per HEAD blob via
  tree-sitter (TS/TSX/JS, Rust, Python, Go, Java, C#, Ruby, C) in one parse; keyed by `blob_sha`,
  recomputed over full HEAD each index (cheap, ~0.17 ms/blob — ).
- `src/incremental.rs` + `src/read.rs` — delta walk + read prior Parquet back +
  re-blame only changed files.
- `src/refs.rs` — branch-topology ref selection.

### numstat: bit-exact, via git's own xdiff (libgit2)

Line counts are diff-algorithm-dependent (git gives 23/2 with myers, 24/3 with
histogram for the same change), so a reimplemented differ can't match git
bit-for-bit. We compute line diffs with **git's actual xdiff via libgit2** (git's
defaults: Myers + indent heuristic), so `numstat` equals `git --numstat` exactly.
(libgit2 is GPLv2 **with a linking exception** — safe to link from this Apache-2.0
engine; vendoring git's raw GPLv2 xdiff would not be.)

### renames: the differentiator (`rename_recall_delta`)

Exact renames are a blob-SHA hash join. Inexact/edited renames are detected
content-first, line-oriented: each blob → a line multiset + a 128-wide MinHash
signature (cached per blob SHA); similarity = `common_lines / max(lines)`;
candidates are all leftover D×A at/under git's 1000-file `renameLimit`, and
LSH-banded beyond it — **no cap**, exactly the big-refactor case git gives up on.
On eos-monorepo we find **every** rename git finds (`miss=0`) plus 4 it misses.
`bench` publishes the two-way delta rather than claiming bit-parity.

### blame: git-CLI by default, native (gitoxide) opt-in

Blame is the one order-dependent fact, and matching `git blame` means matching
git's intricate merge parent-selection. Two implementations, both parallel across
HEAD files (rayon):

- **Default — git-CLI** (`git blame --porcelain`): **100%** agreement (it *is*
  git blame), and **measured faster at scale** — git's C blame beats a from-scratch
  per-file history walk on a large repo, which more than offsets the subprocess
  cost. Two throughput wins on top: (a) **`--porcelain`, not `--line-porcelain`** —
  both give one `<sha>` header per line (all we parse), but `--porcelain` prints
  each commit's author/date block once, not per line (~5-6× less output on a file
  dominated by few commits); (b) a **single-add fast path** — a file added in one
  commit and never touched has a trivial blame (every line = that commit), so we
  read the HEAD blob and count lines instead of spawning `git blame`. On eos ~half
  the files qualify, roughly halving the cold index — verified bit-exact vs
  `git blame` on every one of them.
- **Opt-in, experimental — native gitoxide** (`gix::blame`, `EOS_BLAME=native`):
  in-process, no subprocess, one shared object store, **identical coverage** (same
  files and line counts, never a gap). But **measured worse than git-CLI on every
  axis at scale** and only accurate on small repos:

  | repo | agreement vs git | speed vs git-CLI | peak RAM |
  |---|---|---|---|
  | eos-monorepo (811 commits) | 99.7% | ~1.5× faster | similar |
  | vscode (116k commits) | **95.4%** | **2.3× slower** | **3× (3.4 GB)** |

  The divergence is gitoxide's diff/rename line-matching differing from git's; it
  compounds on merge-heavy history, dropping below the ≥99.5% bar. **Not
  recommended** — kept behind the flag for git-binary-free operation on small
  repos and as a reference.

**git-CLI is the default and the recommendation** — it wins on correctness *and*
large-repo speed and memory. The PR's true "blame as a fold" (a piece-table
interval map walked once — `O(diff volume)`) is a separate, larger effort;
`gix::blame` is per-file, not that fold. Incremental re-blames only changed files
with the selected implementation.

### blame snapshots: ownership over time (opt-in)

HEAD blame answers "who owns each line *now*". A **snapshot** answers it at a
point in the *past* — the tree as it stood at a tag, a release, or any chosen
revision — so ownership, bus factor and code age can be tracked as a time series,
not just at the tip. Written to `blame_snapshots` (path-keyed: a file present at
an old snapshot may be gone by HEAD).

Re-blaming every commit's whole tree is `O(commits × files)` and infeasible on a
large history, so snapshots are a **bounded, opt-in** set of revisions, selected
by `EOS_SNAPSHOTS` (unset ⇒ no snapshots, zero extra work):

```bash
EOS_SNAPSHOTS=tags       gitindex index .   # one snapshot per tag (releases)
EOS_SNAPSHOTS=tags:10    gitindex index .   # cap at the 10 most recent tags
EOS_SNAPSHOTS=v1.0,v2.0  gitindex index .   # exactly these revisions
```

Tags are peeled to commits (annotated or lightweight), deduped, most-recent
first, and capped at `EOS_SNAPSHOTS_MAX` (default 20; anything dropped is logged,
never silently). Each snapshot is an exact `git blame <sha>` over that revision's
tree, so git stays the oracle — the bench verifies sampled snapshot lines against
`git blame <snapshot_sha>` (100% on eos-monorepo) and checks every `snapshot_sha`
is a commit with contiguous line numbers. Edge cases handled: a tag on a
tree/blob (skipped), an unresolvable rev (warned + skipped), files deleted or
renamed since the snapshot (blamed at the snapshot regardless), submodule/binary
files (skipped), and incremental pushes (snapshots are a full-index artifact and
are preserved, not clobbered). The `eng/marts` dbt models can fork this into an
`ownership_over_time` view.

### ownership: materialized at ingest

"Who owns this file" is a blame → commits → authors → identities join every
consumer would otherwise rewrite. The engine folds it once at ingest into
`file_ownership` (one row per HEAD file × person: `owned_lines`, `file_lines`,
`ownership_share`), so ownership, dominant-owner and bus-factor queries are a
single table read — and it's **identity-resolved**, so a person's several emails
don't split their ownership. It's derived, not a git primitive, so the bench
checks it the honest way: `file_ownership` must **exactly equal** the definitional
recomputation from `blame + commits + authors` (any missing/extra/miscounted row
fails), and `file_lines`/`ownership_share` must be internally consistent
(`share = owned/file_lines ∈ (0,1]`, per-file shares sum to ≤ 1). Recomputed in
both the full and incremental paths — the incremental-equality test compares it by
natural keys (path + email) too, so a push yields the same ownership as a full
reindex. A line whose origin commit is outside the commit set is counted in
`file_lines` but owned by no one (≈ 0 on a full history), so shares can sum to
slightly under 1 — surfaced, not hidden.

### incremental ()

On a fast-forward push, walk only the new commits `(old, new]`, read the previous
Parquet back, and re-blame only files whose content changed — unchanged files
keep their previous blame (content-addressing: an unchanged HEAD blob had no
delta commit touch it). Non-fast-forward falls back to full; `--full` forces it.
Verified identical to a full reindex by `bench incremental`.

### identity resolution — union-find over git signals

Authors (one per email) are merged into people by **union-find** over
git-derivable signals: a shared display name (`name-exact`, conf 0.9) and a shared
GitHub-noreply login (`forge-noreply`, conf 0.95 — `1234+login@users.noreply.
github.com` encodes the login, no forge API needed). `identity_aliases` records,
per email, the `method` and `confidence` that linked it (`sole` when unmerged);
`identities` carries the canonical name/email and the cluster's min confidence.

**Weak signals go to a review queue, not an automatic merge.** Name *similarity*
(token-Jaccard ≥ 0.5, blocked by first name-token) and a shared non-generic email
local-part become `identity_reviews` suggestions — a human/consumer confirms
before merging, so a low-confidence guess never silently corrupts a downstream
join. No git oracle (identity is heuristic); `bench` checks structural integrity
(every author linked once, methods/confidences valid, reviews reference distinct
identities). Forge-login-from-API (SDR  signal 1) is out of scope (needs L2).

### symbols (L3, first slice)

Definitions per HEAD blob via tree-sitter — `name, kind, start/end_line, lang`,
keyed by `blob_sha` (the content address). **Languages: TS/TSX/JS, Rust, Python,
Go, Java, C#, Ruby, C** — one shared harness (`Lang` → grammar + a def query + a ref
query); adding a language is a grammar dep plus two queries. Kinds are normalized
to language-neutral labels (`function`, `method`, `class`, `interface`, `struct`,
`enum`, `record`, `type`, `const`, `property`, `constructor`, `trait`, `macro`, …)
so cross-language queries work. There is **no git oracle** for symbols
(git doesn't parse code), so the check is structural: line ranges and file
references are sane, and every `blob_sha` is verified to equal the file's real
HEAD blob (`git rev-parse HEAD:<path>`) — git *is* the oracle for the
content-address. tree-sitter is the reference for the symbols themselves, and
per-language extraction is unit-tested (`cargo test`, no repo needed).
Recomputed over full HEAD each index (extraction is ~0.17 ms/blob — ), so
it needs no incremental delta.

### symbol_refs (L3, references slice)

*Lexical* usage sites — bare calls (`foo()`, `ref_kind='call'`), member calls
(`x.foo()`, `'method'`, noisier), `new`, and Rust macros — filtered to names that
are **defined somewhere in the repo**, giving a candidate call-graph.

**Import resolution** narrows candidate → binding, filling `def_file_id` so a call
to a name defined in five files points at the **one** file it actually comes from.
Two resolvers, because languages bind differently:

- **Path-based (TS/TSX/JS)** — a local name is bound to a *relative module*; the
  resolver follows the import (relative specifiers, extension / `index`
  resolution, and **tsconfig `paths` aliases** — `@/x` → the mapped file). On eos-monorepo ~26% of refs resolve, 99.5% of those to a file that
  really defines the name (the rest are re-export barrels, out of scope).
- **Scope-based (Go, Java, C#, Rust, Python)** — imports name a
  *package/namespace/module*, not a single symbol, so a reference resolves to the
  file defining the name within a **visible scope**: a repo-unique name resolves
  outright, otherwise the name must be unique among the file's own + imported
  scopes. Each language derives its scope its own way:
  - **Go** — a package is a directory; a file sees its own package (same-package,
    cross-file). Cross-package `pkg.X` selectors resolve too: each `import` (via
    the applicable `go.mod` module path, with aliases) maps a package qualifier to
    its repo directory, and `pkg.Name` resolves `Name` in *that* package — so two
    packages both defining `New()` each resolve to their own.
  - **Java** — own `package` + each `import a.b.C` (→ `a.b`) / `import a.b.*`.
  - **C#** — own `namespace` + each `using A.B`.
  - **Rust** — the file's crate-absolute module (`<crate>::a::b` from `src/a/b.rs`),
    plus `use` paths with `crate`/`self`/`super` resolved (external crates skipped).
  - **Python** — the file's dotted module (`a.b.c`), plus `from …` modules
    (absolute, or relative `.mod` / `..pkg` resolved against the file's package).

  This disambiguates correctly — e.g. two Go packages each defining `connect()`, or
  two Rust modules each defining `open()`, resolve to *their own* scope's file,
  never across the boundary.

**Type-based (member calls `x.foo()`)** — a member call is name-only unless the
receiver's *type* is known. A slice of type resolution infers it from a few
syntactic sources (no full type inference), then resolves `foo` among that type's
methods: `this`/`self` → the enclosing type; `new T()` / `T{}` → the constructed
type; a variable that is a **typed parameter** of the enclosing function → its
declared type; and a **local variable** whose type is known from its declaration —
`const x = new Foo()` or `Foo x = new Foo()` / `let x: Foo` (TS/JS + Java) → `Foo`.
Method **ownership** is the enclosing type (containment) for OO languages (TS,
Java, C#, Python) or the receiver type for Go methods. So `new Helper().help()`,
`h.help()` for a param `h: Helper`, `const h = new Helper(); h.help()`, and
`this.help()` all resolve to `Helper`'s file. Preferred over the name-based
fallback when it succeeds. Out of scope for this slice: field/getter chains,
generics, C#/Python/Go local-var types, and Rust `impl` methods.

Unresolved refs (`def_file_id IS NULL`) are member calls on untyped/complex
receivers, globals, Go selectors on non-package receivers, Rust inline-`mod` items
or re-exports, and `import a.b` (Python attribute-style, unbound). The
`references_structural` oracle asserts every resolved target really defines the
name, and the resolvers are unit-tested (scope + import parsing, the
unique/visible/ambiguous rules, and member-call receiver classification).

Still **not** full binding resolution — no aliased re-exports (`export { A as B }`),
field/getter chains, or generics. Those complete the **semantic** L3 (SCIP-grade),
where the  cross-tenant cache payoff lands (). No git oracle (git
doesn't parse code); the structural check asserts every reference resolves to a
symbol, every `def_file_id` target actually defines the name, and `blob_sha`
matches git's HEAD.

### symbol_edges (GRAIL — the symbol reference graph)

`symbol_refs` is usage sites; **GRAIL** lifts them to a symbol→symbol graph by
attributing each reference to the definition that **encloses** it. The caller is
the *innermost* symbol in the same file whose `[start_line, end_line]` contains
the call line (so a call inside a method binds to that method, not its class); the
callee is the referenced name, carried with `def_file_id` and pinned to a single
symbol (`dst_start_line`) when that `(file, name)` is unambiguous. That gives
impact analysis ("what calls X"), dependency ("what does Y use"), recursion
(self-edges), and dead-symbol candidates (no inbound edge). References at
module/top-level scope have no caller symbol and are excluded — they stay in
`symbol_refs`. `ref_kind` rides along so a consumer can keep the high-precision
`call`/`new` edges and drop noisier `method` ones. On eos-monorepo: **16.5k edges
from 2.6k caller symbols**, ~24% file-resolved, ~23% pinned to one symbol.

Derived, not a git primitive — so the oracle is definitional: `symbol_edges` must
**exactly equal** the enclosing-symbol recomputation over `symbols + symbol_refs`
(a range-containment join, innermost per reference), and every edge's caller must
exist and contain the line, every pinned callee name exactly one symbol. Built in
both index paths; the incremental-equality test compares it by paths too.

Deferred: type-based resolution, path aliases, re-exports, SCIP semantics.

### content chunking / dedup (FastCDC)

Content-defined chunking splits a blob at boundaries chosen by a rolling *gear*
hash, not fixed offsets — so an edit changes only the chunks it touches and the
chunks after it keep the same boundaries **and the same content hash**. Hash each
chunk and identical content is stored once: `chunks` is the deduped store (hash,
size, ref_count), `blob_chunks` each blob's ordered membership (a blob = its
chunks concatenated). *Normalized* FastCDC — a stricter mask below the average
size, looser above — pulls sizes toward the average and bounds every chunk to
`[avg/4, avg·8]`; the gear table and masks come from a fixed seed, so chunking is
reproducible on any machine. The chunk address is xxh3-128 (fast; swap in SHA-256
for an adversarial store).

Opt-in via `EOS_CHUNK` (unset ⇒ nothing, zero cost):

```bash
EOS_CHUNK=1        gitindex index .   # HEAD blobs, 8 KiB average
EOS_CHUNK=32       gitindex index .   # HEAD blobs, 32 KiB average
EOS_CHUNK=history  gitindex index .   # every blob version in the odb
```

Within one HEAD snapshot there's little duplicate content (~1.0×) — the honest
result. Dedup lives **across versions**: `history` chunks every blob object, where
a file edited N times is N whole blobs but shares most of its chunks. On
eos-monorepo `history` measures **6.3k blobs → 12.8k chunks (9.2k unique), 75.8 →
52.3 MiB, dedup 1.45×**. No git oracle (git stores whole blobs); the bench checks
the properties that make dedup sound — chunks tile each blob exactly (offsets are
the running size sum; totals match the real blob size via git), the address is
consistent (a hash never has two sizes, ref_counts are right), and sizes stay in
bounds — while the Rust unit tests cover exact-byte reconstruction, determinism,
size bounds, and the boundary-shift resistance that makes chunks survive edits.
Full-index artifact; a push preserves prior chunks rather than clobbering them.

## Status

L1 core is complete (–3.12) and green against git on every oracle; the
L3 **symbols** (definitions) and **symbol_refs** (lexical references) slices are
landed and structurally verified. Deferred by design: semantic L3 (resolved
references / SCIP), forge/social layer (L2), the hosted service.
