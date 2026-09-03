#!/usr/bin/env node

// src/cli.ts
import { resolve as resolve2 } from "path";
import { mkdirSync as mkdirSync3 } from "fs";
import { spawnSync as spawnSync2 } from "child_process";

// src/common.ts
import { readFileSync, readdirSync, statSync, mkdirSync, existsSync } from "fs";
import { dirname, isAbsolute, join, resolve } from "path";
import { fileURLToPath } from "url";
var HERE = dirname(fileURLToPath(import.meta.url));
var ROOT = resolve(HERE, "..");
var DATA = process.env.EOS_DATA_DIR ? resolve(process.env.EOS_DATA_DIR) : resolve(ROOT, ".data");
var GITINDEX = process.env.GITINDEX_BIN ?? resolve(ROOT, "../gitindex/target/release/gitindex");
var SEMANTIC_DIR = process.env.EOS_SEMANTIC_DIR ?? resolve(ROOT, "../semantic");
var SEMANTIC_BIN = process.env.EOS_SEMANTIC_BIN ?? join(SEMANTIC_DIR, "bin/semantic.ts");
var ORG_PKG = process.env.EOS_ORG_DIR ?? resolve(ROOT, "../org");
var ORG_AGGREGATE = join(ORG_PKG, "src/aggregate.ts");
var ORG_VERIFY = join(ORG_PKG, "src/verify.ts");
var ORG_OUT = join(DATA, "org");
var PLAN_LEVEL = {
  free: "basic",
  starter: "mid",
  pro: "mid",
  enterprise: "high"
};
var LEVELS = ["basic", "mid", "high"];
function indexLevelFor(t) {
  if (t.indexLevel !== void 0) {
    if (!LEVELS.includes(t.indexLevel))
      throw new Error(`tenant '${t.id}': invalid indexLevel '${t.indexLevel}' (basic|mid|high)`);
    return t.indexLevel;
  }
  if (t.plan !== void 0) {
    const lvl = PLAN_LEVEL[t.plan];
    if (!lvl) throw new Error(`tenant '${t.id}': invalid plan '${t.plan}' (free|starter|pro|enterprise)`);
    return lvl;
  }
  return void 0;
}
function loadTenants() {
  const local = process.env.EOS_LOCAL_REPO ? [{ id: "local", name: `local: ${process.env.EOS_LOCAL_REPO}`, path: process.env.EOS_LOCAL_REPO, visibility: "public" }] : [];
  const f = join(ROOT, "tenants.json");
  const file = existsSync(f) ? JSON.parse(readFileSync(f, "utf8")).tenants : [];
  return [...local, ...file];
}
function findTenant(id) {
  const t = loadTenants().find((x) => x.id === id);
  if (!t) throw new Error(`unknown tenant '${id}' (see tenants.json)`);
  return t;
}
function tenantForRepo(url) {
  const norm = (s) => s.replace(/\.git$/, "").replace(/\/$/, "").toLowerCase();
  const u = norm(url);
  return loadTenants().find((t) => t.url && norm(t.url) === u);
}
var mirrorDir = (id) => join(DATA, "mirrors", id);
var indexDir = (id) => join(DATA, "index", id);
var meterFile = (id) => join(DATA, "meters", `${id}.json`);
var pinsFile = (id) => join(DATA, "pins", `${id}.json`);
var keysFile = () => join(DATA, "keys.json");
function cloneSource(t) {
  if (t.path) return isAbsolute(t.path) ? t.path : resolve(ROOT, t.path);
  if (t.url) return t.url;
  throw new Error(`tenant '${t.id}' needs a path or url`);
}
function ensureDataDirs() {
  for (const d of ["mirrors", "index", "meters", "pins"]) mkdirSync(join(DATA, d), { recursive: true });
}
function dirSize(dir) {
  let total = 0;
  let entries;
  try {
    entries = readdirSync(dir);
  } catch {
    return 0;
  }
  for (const e of entries) {
    const p = join(dir, e);
    const s = statSync(p);
    total += s.isDirectory() ? dirSize(p) : s.size;
  }
  return total;
}

// src/server.ts
import { createServer } from "http";
import { existsSync as existsSync6, readFileSync as readFileSync5 } from "fs";
import { join as join4 } from "path";

// src/pool.ts
import { join as join2 } from "path";
import { existsSync as existsSync2, readFileSync as readFileSync2 } from "fs";
import { DuckDBInstance } from "@duckdb/node-api";
var POOL_MAX = Number(process.env.EOS_POOL_MAX ?? 8);
function manifestHead(id) {
  const f = join2(indexDir(id), "manifest.json");
  if (!existsSync2(f)) return "none";
  try {
    return JSON.parse(readFileSync2(f, "utf8")).head_sha ?? "none";
  } catch {
    return "none";
  }
}
var cache = /* @__PURE__ */ new Map();
async function build(id, setup, head) {
  const instance = await DuckDBInstance.create(":memory:");
  const first = await instance.connect();
  await setup(first);
  const idle = [first];
  const waiters = [];
  let size = 1;
  return {
    head,
    async acquire() {
      const c = idle.pop();
      if (c) return c;
      if (size < POOL_MAX) {
        size++;
        return instance.connect();
      }
      return new Promise((resolve3) => waiters.push(resolve3));
    },
    release(c) {
      const w = waiters.shift();
      if (w) w(c);
      else idle.push(c);
    }
  };
}
async function withConn(id, setup, fn) {
  const head = manifestHead(id);
  let db = cache.get(id);
  if (!db || db.head !== head) {
    db = await build(id, setup, head);
    cache.set(id, db);
  }
  const conn = await db.acquire();
  try {
    return await fn(conn);
  } finally {
    db.release(conn);
  }
}

// src/mirror.ts
import { existsSync as existsSync4 } from "fs";
import { spawnSync } from "child_process";
import { join as join3 } from "path";

// src/jsonstore.ts
import { closeSync, existsSync as existsSync3, fsyncSync, mkdirSync as mkdirSync2, openSync, readFileSync as readFileSync3, renameSync, writeFileSync } from "fs";
import { dirname as dirname2 } from "path";
function readJson(path, fallback) {
  if (!existsSync3(path)) return fallback;
  const text = readFileSync3(path, "utf8");
  try {
    return JSON.parse(text);
  } catch (e) {
    throw new Error(`corrupt state file ${path} \u2014 refusing to overwrite with defaults: ${e.message}`);
  }
}
function writeJsonAtomic(path, value) {
  mkdirSync2(dirname2(path), { recursive: true });
  const tmp = `${path}.${process.pid}.${randomSuffix()}.tmp`;
  writeFileSync(tmp, JSON.stringify(value, null, 2));
  const fd = openSync(tmp, "r");
  try {
    fsyncSync(fd);
  } finally {
    closeSync(fd);
  }
  renameSync(tmp, path);
}
var counter = 0;
function randomSuffix() {
  counter = counter + 1 & 65535;
  return counter.toString(16);
}

// src/meter.ts
function read(id) {
  return readJson(meterFile(id), {
    tenant: id,
    index_seconds_total: 0,
    index_runs: 0,
    storage_bytes: 0,
    query_count: 0,
    query_ms_total: 0
  });
}
function write(m) {
  writeJsonAtomic(meterFile(m.tenant), m);
}
function recordIndex(id, seconds, storageBytes, head) {
  const m = read(id);
  m.index_seconds_total += seconds;
  m.index_runs += 1;
  m.storage_bytes = storageBytes;
  m.last_indexed_at = Math.floor(Date.now() / 1e3);
  m.head_sha = head;
  write(m);
}
function recordQuery(id, ms) {
  const m = read(id);
  m.query_count += 1;
  m.query_ms_total += ms;
  write(m);
}

// src/mirror.ts
var DEDUP_CACHE = join3(DATA, "cache");
function run(cmd, args, cwd, env) {
  const r = spawnSync(cmd, args, { encoding: "utf8", maxBuffer: 1 << 30, cwd, env: env ? { ...process.env, ...env } : void 0 });
  return { code: r.status ?? -1, out: r.stdout ?? "", err: r.stderr ?? "" };
}
function resolveSemantic(t, out) {
  const wt = join3(DATA, "worktrees", t.id);
  run("git", ["-C", mirrorDir(t.id), "worktree", "remove", "--force", wt]);
  const add = run("git", ["-C", mirrorDir(t.id), "worktree", "add", "--force", "--detach", wt, "HEAD"]);
  if (add.code !== 0) {
    console.log(`  semantic: worktree failed, skipping (${add.err.trim().slice(0, 80)})`);
    return;
  }
  try {
    const r = run("node", ["--experimental-strip-types", SEMANTIC_BIN, "resolve", "--repo", wt, "--index", out], SEMANTIC_DIR);
    process.stderr.write(r.code === 0 ? "  " + r.out.trim() + "\n" : `  semantic failed: ${r.err.trim().slice(0, 160)}
`);
  } finally {
    run("git", ["-C", mirrorDir(t.id), "worktree", "remove", "--force", wt]);
  }
}
function refreshMirror(t) {
  const dir = mirrorDir(t.id);
  if (!existsSync4(dir)) {
    const src = cloneSource(t);
    console.log(`  cloning mirror ${src} -> mirrors/${t.id}`);
    const r = run("git", ["clone", "--mirror", "--quiet", src, dir]);
    if (r.code !== 0) throw new Error(`clone failed: ${r.err}`);
  } else {
    const r = run("git", ["-C", dir, "remote", "update", "--prune"]);
    if (r.code !== 0) throw new Error(`fetch failed: ${r.err}`);
  }
}
function sync(id) {
  ensureDataDirs();
  const t = findTenant(id);
  console.log(`\u25B6 sync ${t.id} (${t.name})`);
  refreshMirror(t);
  const out = indexDir(t.id);
  const level = indexLevelFor(t);
  if (level) console.log(`  index level: ${level}${t.indexLevel ? " (explicit)" : ` (plan: ${t.plan})`}`);
  const t0 = Date.now();
  const r = run(GITINDEX, ["index", mirrorDir(t.id), "--out", out, "--cache", DEDUP_CACHE], void 0, level ? { EOS_INDEX_LEVEL: level } : void 0);
  const secs = (Date.now() - t0) / 1e3;
  if (r.code !== 0) throw new Error(`gitindex failed: ${r.err}`);
  process.stderr.write(r.err.split("\n").filter((l) => l.includes("index:") || l.includes("incremental:") || l.includes("dedup")).map((l) => "  " + l.trim()).join("\n") + "\n");
  if (t.semantic) resolveSemantic(t, out);
  const head = run("git", ["-C", mirrorDir(t.id), "rev-parse", "HEAD"]).out.trim();
  const bytes = dirSize(out);
  recordIndex(t.id, secs, bytes, head);
  console.log(`  indexed in ${secs.toFixed(2)}s \xB7 storage ${(bytes / 1024).toFixed(0)}KB \xB7 head ${head.slice(0, 10)}`);
}

// src/pins.ts
var DRIFT = 1.5;
var DEFAULT_SLO_MS = 2e3;
function load(id) {
  return readJson(pinsFile(id), {});
}
function save(id, pins) {
  writeJsonAtomic(pinsFile(id), pins);
}
function list(id) {
  return Object.values(load(id)).sort((a, b) => a.name.localeCompare(b.name));
}
function get(id, name) {
  return load(id)[name];
}
function pin(id, name, sql, baseCostRows, opts = {}, now = 0) {
  if (!/^[a-zA-Z0-9_-]{1,64}$/.test(name)) throw new Error("pin name must match [a-zA-Z0-9_-]{1,64}");
  const rec = {
    name,
    sql,
    base_cost_rows: baseCostRows,
    cost_ceiling: Math.max(1, Math.floor(opts.cost_ceiling ?? baseCostRows * DRIFT)),
    slo_ms: Math.max(1, Math.floor(opts.slo_ms ?? DEFAULT_SLO_MS)),
    created_at: now
  };
  const pins = load(id);
  pins[name] = rec;
  save(id, pins);
  return rec;
}
function remove(id, name) {
  const pins = load(id);
  if (!(name in pins)) return false;
  delete pins[name];
  save(id, pins);
  return true;
}

// src/auth.ts
import { createHash, randomBytes, timingSafeEqual } from "crypto";
import { existsSync as existsSync5, readFileSync as readFileSync4 } from "fs";
function hashKey(raw) {
  return createHash("sha256").update(raw.trim()).digest("hex");
}
function loadKeys() {
  const out = {};
  const f = keysFile();
  if (existsSync5(f)) {
    try {
      Object.assign(out, JSON.parse(readFileSync4(f, "utf8")));
    } catch {
    }
  }
  const env = process.env.EOS_API_KEYS;
  if (env) {
    for (const pair of env.split(",")) {
      const [raw, tenant] = pair.split(":");
      if (raw && tenant) out[hashKey(raw)] = { tenant: tenant.trim(), label: "env", created_at: 0 };
    }
  }
  return out;
}
function findKey(rawKey) {
  const target = hashKey(rawKey);
  const keys = loadKeys();
  const tb = Buffer.from(target);
  for (const [h, rec] of Object.entries(keys)) {
    const hb = Buffer.from(h);
    if (hb.length === tb.length && timingSafeEqual(hb, tb)) return rec;
  }
  return void 0;
}
function extractKey(headers) {
  const auth = headers["authorization"];
  const bearer = Array.isArray(auth) ? auth[0] : auth;
  if (bearer && /^bearer\s+/i.test(bearer)) return bearer.replace(/^bearer\s+/i, "").trim();
  const x = headers["x-api-key"];
  const xk = Array.isArray(x) ? x[0] : x;
  return xk ? xk.trim() : null;
}
var AuthError = class extends Error {
  status;
  constructor(status, message) {
    super(message);
    this.status = status;
  }
};
function authorize(headers, tenant, opts) {
  const raw = extractKey(headers);
  if (!raw) {
    if (!opts.write && tenant.visibility === "public") return { keyId: null };
    throw new AuthError(401, "missing API key (Authorization: Bearer <key> or x-api-key)");
  }
  const rec = findKey(raw);
  if (!rec) throw new AuthError(403, "invalid API key");
  if (rec.tenant !== tenant.id) throw new AuthError(403, `API key is not scoped to tenant '${tenant.id}'`);
  return { keyId: hashKey(raw).slice(0, 16) };
}

// src/ratelimit.ts
var CAPACITY = Number(process.env.EOS_RATE_BURST ?? 60);
var PER_MINUTE = Number(process.env.EOS_RATE_PER_MIN ?? 120);
var REFILL_PER_MS = PER_MINUTE / 6e4;
var buckets = /* @__PURE__ */ new Map();
function take(key, now = Date.now()) {
  const b = buckets.get(key) ?? { tokens: CAPACITY, ts: now };
  b.tokens = Math.min(CAPACITY, b.tokens + (now - b.ts) * REFILL_PER_MS);
  b.ts = now;
  if (b.tokens < 1) {
    buckets.set(key, b);
    return { ok: false, retry_after_ms: Math.ceil((1 - b.tokens) / REFILL_PER_MS) };
  }
  b.tokens -= 1;
  buckets.set(key, b);
  return { ok: true };
}

// src/obs.ts
import { randomBytes as randomBytes2 } from "crypto";
function log(level, msg, fields = {}) {
  const line = JSON.stringify({ ts: (/* @__PURE__ */ new Date()).toISOString(), level, msg, ...fields });
  (level === "error" ? process.stderr : process.stdout).write(line + "\n");
}
function newRequestId() {
  return randomBytes2(8).toString("hex");
}
var requests = 0;
var errors = 0;
var byStatus = /* @__PURE__ */ new Map();
var latencies = [];
var MAX_SAMPLES = 2e3;
function percentile(sorted, p) {
  if (sorted.length === 0) return 0;
  const i = Math.min(sorted.length - 1, Math.floor(p / 100 * sorted.length));
  return sorted[i];
}
function recordRequest(r) {
  requests++;
  if (r.status >= 500) errors++;
  byStatus.set(r.status, (byStatus.get(r.status) ?? 0) + 1);
  latencies.push(r.ms);
  if (latencies.length > MAX_SAMPLES) latencies.shift();
  log(r.status >= 500 ? "error" : "info", "request", {
    req_id: r.reqId,
    method: r.method,
    path: r.path,
    status: r.status,
    ms: r.ms,
    tenant: r.tenant
  });
}
function snapshot() {
  const sorted = [...latencies].sort((a, b) => a - b);
  return {
    requests_total: requests,
    errors_total: errors,
    by_status: Object.fromEntries(byStatus),
    latency_ms: { p50: percentile(sorted, 50), p95: percentile(sorted, 95), p99: percentile(sorted, 99), n: sorted.length },
    uptime_s: Math.round(process.uptime())
  };
}
var reporter = (err, ctx) => {
  log("error", "unhandled", { error: err instanceof Error ? err.message : String(err), stack: err instanceof Error ? err.stack : void 0, ...ctx });
};
function reportError(err, ctx = {}) {
  try {
    reporter(err, ctx);
  } catch {
  }
}

// src/surfaces.ts
var lit = (s) => `'${String(s).replace(/'/g, "''")}'`;
var scope = (col, p) => `(${col} = ${lit(p)} OR ${col} LIKE ${lit(p + "/%")})`;
function clampInt(v, def, min, max) {
  if (v == null || v === "") return def;
  const n = Number(v);
  if (!Number.isFinite(n)) throw new Error(`expected an integer, got '${v}'`);
  return Math.max(min, Math.min(max, Math.trunc(n)));
}
function parseParams(q) {
  return {
    path: q.get("path")?.trim() || void 0,
    query: q.get("query")?.trim() || void 0,
    days: clampInt(q.get("days"), 90, 1, 3650),
    limit: clampInt(q.get("limit"), 20, 1, 500)
  };
}
function requirePath(p) {
  if (!p.path) throw new Error("this surface requires a `path` query parameter (a file or directory)");
  return p.path;
}
function requireQuery(p) {
  if (!p.query) throw new Error("this surface requires a `query` query parameter (a search term)");
  return p.query;
}
var SURFACES = {
  // Who owns a file or area, by blame lines — with each owner's share and how
  // many files they dominate. The "who should I ask about this?" answer.
  ownership: {
    describe: "Top owners of a file or directory, by blame lines (share + files).",
    needsPath: true,
    build(p) {
      const path = requirePath(p);
      return `
        WITH area AS (SELECT * FROM v_blame WHERE ${scope("path", path)}),
        per AS (
          SELECT author_name, author_email,
                 count(*)                AS owned_lines,
                 count(DISTINCT path)    AS files,
                 max(committed_at_epoch) AS last_touch_epoch
          FROM area
          GROUP BY author_name, author_email
        )
        SELECT author_name, author_email, owned_lines, files,
               round(owned_lines::DOUBLE / NULLIF(sum(owned_lines) OVER (), 0), 3) AS share,
               last_touch_epoch
        FROM per
        ORDER BY owned_lines DESC
        LIMIT ${p.limit}`;
    }
  },
  // Suggested reviewers for a change to this path — git-native: the people who
  // most own it (blame) and have most recently changed it, with the evidence for
  // each. No PR-review data needed; this is "who knows this code".
  reviewers: {
    describe: "Suggested reviewers for a path \u2014 ranked by ownership + recent changes, with evidence.",
    needsPath: true,
    build(p) {
      const path = requirePath(p);
      return `
        WITH own AS (
          SELECT author_email, any_value(author_name) AS author_name, count(*) AS owned_lines
          FROM v_blame WHERE ${scope("path", path)} GROUP BY author_email
        ),
        recent AS (
          SELECT author_email, any_value(author_name) AS author_name,
                 count(*) AS recent_changes, max(committed_at_epoch) AS last_touch_epoch
          FROM v_changes WHERE ${scope("path", path)} GROUP BY author_email
        )
        SELECT coalesce(o.author_email, r.author_email) AS author_email,
               coalesce(o.author_name,  r.author_name)  AS author_name,
               coalesce(o.owned_lines, 0)               AS owned_lines,
               coalesce(r.recent_changes, 0)            AS recent_changes,
               r.last_touch_epoch
        FROM own o FULL OUTER JOIN recent r ON o.author_email = r.author_email
        ORDER BY owned_lines DESC, recent_changes DESC
        LIMIT ${p.limit}`;
    }
  },
  // Blast radius: files that have historically changed together with this path
  // in the same commit — the coupling an import graph can't see. "What else
  // tends to move when I touch this?"
  blast_radius: {
    describe: "Files that historically change together with a path (temporal coupling).",
    needsPath: true,
    build(p) {
      const path = requirePath(p);
      return `
        WITH tc AS (SELECT DISTINCT commit_sha FROM v_changes WHERE ${scope("path", path)})
        SELECT ch.path,
               count(DISTINCT ch.commit_sha) AS co_changes,
               max(ch.committed_at_epoch)    AS last_co_change_epoch
        FROM v_changes ch JOIN tc ON ch.commit_sha = tc.commit_sha
        WHERE NOT ${scope("ch.path", path)}
        GROUP BY ch.path
        ORDER BY co_changes DESC
        LIMIT ${p.limit}`;
    }
  },
  // Recent change volume over a window (anchored to the newest commit in the
  // index, not wall-clock, so it is honest even if the index lags). Optional
  // `path` scopes it to a file/area. "What's moving, and how much?"
  activity: {
    describe: "Change volume over the last N days of history (optional path scope).",
    needsPath: false,
    build(p) {
      const where = p.path ? `AND ${scope("ch.path", p.path)}` : "";
      return `
        WITH b AS (SELECT max(committed_at_epoch) AS latest FROM v_changes),
        scope AS (
          SELECT ch.* FROM v_changes ch, b
          WHERE ch.committed_at_epoch >= b.latest - ${p.days} * 86400 ${where}
        )
        SELECT count(DISTINCT commit_sha)       AS commits,
               count(*)                         AS file_changes,
               count(DISTINCT path)             AS files,
               count(DISTINCT author_email)     AS authors,
               coalesce(sum(added_lines), 0)    AS lines_added,
               coalesce(sum(removed_lines), 0)  AS lines_removed,
               (SELECT latest FROM b)                        AS window_end_epoch,
               (SELECT latest FROM b) - ${p.days} * 86400    AS window_start_epoch
        FROM scope`;
    }
  },
  // Per-person footprint: commits, files touched, and active span. Optional
  // `path` scopes it to an area ("who works on the auth module?").
  contributors: {
    describe: "Per-person footprint \u2014 commits, files, and active span (optional path scope).",
    needsPath: false,
    build(p) {
      const where = p.path ? `WHERE ${scope("path", p.path)}` : "";
      return `
        SELECT author_email, any_value(author_name) AS author_name,
               count(DISTINCT commit_sha) AS commits,
               count(*)                   AS file_changes,
               count(DISTINCT path)       AS files,
               min(committed_at_epoch)    AS first_seen_epoch,
               max(committed_at_epoch)    AS last_seen_epoch
        FROM v_changes ${where}
        GROUP BY author_email
        ORDER BY commits DESC
        LIMIT ${p.limit}`;
    }
  },
  // Structural search over the index — symbol definitions, file paths, and commit
  // subjects — ranked and cited to git (path:line or commit sha). The git-native
  // "ask your history / find relevant code": exact, no embeddings. Symbol hits
  // need the L3 tier (symbols present); paths + commits are L1.
  search: {
    describe: "Structural search over symbols, file paths, and commit subjects (cited).",
    needsPath: false,
    needsQuery: true,
    build(p) {
      const term = requireQuery(p).toLowerCase();
      const like = lit("%" + term + "%");
      const eq = lit(term);
      const pre = lit(term + "%");
      return `
        WITH sym AS (
          SELECT 'symbol' AS type, path, name AS title, kind AS detail,
                 CAST(start_line AS BIGINT) AS line, CAST(NULL AS VARCHAR) AS sha,
                 CASE WHEN lower(name) = ${eq} THEN 3 WHEN lower(name) LIKE ${pre} THEN 2 ELSE 1 END AS rank
          FROM v_symbols WHERE lower(name) LIKE ${like}
        ),
        pth AS (
          SELECT 'path' AS type, b.path AS path, b.path AS title, CAST(NULL AS VARCHAR) AS detail,
                 CAST(NULL AS BIGINT) AS line, CAST(NULL AS VARCHAR) AS sha, 1 AS rank
          FROM (SELECT DISTINCT path FROM v_blame) b WHERE lower(b.path) LIKE ${like}
        ),
        cmt AS (
          SELECT 'commit' AS type, CAST(NULL AS VARCHAR) AS path, subject AS title,
                 author_name AS detail, CAST(NULL AS BIGINT) AS line, commit_sha AS sha, 1 AS rank
          FROM v_commits WHERE lower(subject) LIKE ${like}
        )
        SELECT type, title, path, line, sha, detail, rank
        FROM (SELECT * FROM sym UNION ALL SELECT * FROM pth UNION ALL SELECT * FROM cmt)
        ORDER BY rank DESC, type, title
        LIMIT ${p.limit}`;
    }
  },
  // The resolved dependency graph — file → file edges from L3 references (a
  // reference in one file resolved to a definition in another), ranked by how
  // many references cross. Optional `path` scopes to edges touching it. Needs L3.
  graph: {
    describe: "Resolved file-to-file dependency graph from L3 references (optional path scope).",
    needsPath: false,
    build(p) {
      const where = p.path ? `AND (${scope("ref_path", p.path)} OR ${scope("def_path", p.path)})` : "";
      return `
        SELECT ref_path AS from_path, def_path AS to_path, count(*) AS refs
        FROM v_references
        WHERE def_path IS NOT NULL AND def_path <> ref_path ${where}
        GROUP BY ref_path, def_path
        ORDER BY refs DESC
        LIMIT ${p.limit}`;
    }
  },
  // External dependencies declared in HEAD manifests. Optional `query` filters by
  // package name or ecosystem. Served straight from the engine's dependency layer.
  dependencies: {
    describe: "External dependencies from HEAD manifests (optional query filter on name/ecosystem).",
    needsPath: false,
    build(p) {
      const where = p.query ? `WHERE lower(name) LIKE ${lit("%" + p.query.toLowerCase() + "%")} OR lower(ecosystem) = ${lit(p.query.toLowerCase())}` : "";
      return `
        SELECT ecosystem, name, version, scope, manifest_path
        FROM v_dependencies ${where}
        ORDER BY ecosystem, name
        LIMIT ${p.limit}`;
    }
  },
  // Precomputed code-intelligence findings — bus-factor risk, hotspots, hidden
  // coupling, fragile/architecture hubs. Ranked by severity then magnitude.
  // Optional `path` scopes by subject.
  risks: {
    describe: "Code-intelligence findings \u2014 bus-factor, hotspots, hubs, coupling (optional path scope).",
    needsPath: false,
    build(p) {
      const where = p.path ? `WHERE lower(subject) LIKE ${lit("%" + p.path.toLowerCase() + "%")}` : "";
      return `
        SELECT kind, severity, subject, metric, detail
        FROM v_risks ${where}
        ORDER BY CASE severity WHEN 'critical' THEN 0 WHEN 'warning' THEN 1 ELSE 2 END, metric DESC
        LIMIT ${p.limit}`;
    }
  },
  // Technical-debt markers (TODO/FIXME/HACK/XXX/NOTE) in HEAD, with file + line.
  // Optional `path` scopes to a file or area.
  markers: {
    describe: "Technical-debt markers (TODO/FIXME/\u2026) in HEAD, with file:line (optional path scope).",
    needsPath: false,
    build(p) {
      const where = p.path ? `WHERE ${scope("path", p.path)}` : "";
      return `
        SELECT path, line, marker, text
        FROM v_markers ${where}
        ORDER BY path, line
        LIMIT ${p.limit}`;
    }
  }
};
function catalogue() {
  return Object.entries(SURFACES).map(([name, s]) => ({
    surface: name,
    describe: s.describe,
    path: s.needsPath ? "required" : "optional",
    query: s.needsQuery ? "required" : "n/a",
    params: ["path", "query", "days", "limit"]
  }));
}

// src/server.ts
var TABLES = ["commits", "commit_parents", "authors", "files", "commit_files", "blame", "identities", "identity_aliases", "identity_reviews", "symbols", "symbol_refs", "semantic_refs", "dependencies", "insights", "code_markers"];
var readSql = (name) => readFileSync5(new URL(`../${name}`, import.meta.url), "utf8").split(";").map((s) => s.split("\n").filter((l) => !l.trim().startsWith("--")).join("\n").trim()).filter(Boolean);
var VIEWS_L1 = readSql("views.sql");
var VIEWS_L3 = readSql("views_l3.sql");
var VIEWS_SEMANTIC = readSql("views_semantic.sql");
var VIEWS_DEPS = readSql("views_deps.sql");
var VIEWS_RISKS = readSql("views_risks.sql");
var VIEWS_MARKERS = readSql("views_markers.sql");
var VIEW_NAMES_L1 = ["v_commits", "v_changes", "v_blame", "v_ownership"];
var VIEW_NAMES_L3 = ["v_symbols", "v_references", "v_symbol_usage"];
var VIEW_NAMES_SEMANTIC = ["v_semantic"];
var VIEW_NAMES_DEPS = ["v_dependencies"];
var VIEW_NAMES_RISKS = ["v_risks"];
var VIEW_NAMES_MARKERS = ["v_markers"];
var ROW_CAP = 1e4;
var TIMEOUT_MS = 1e4;
var COST_ROW_BUDGET = 1e8;
var BLOCK = /\b(attach|detach|copy|install|load|pragma|call|export|import|insert|update|delete|create|drop|alter|read_csv|read_parquet|read_json|read_text|read_blob|write_|glob)\b/i;
function validate(sql) {
  const s = sql.trim().replace(/;\s*$/, "");
  if (s.includes(";")) throw new Error("only a single statement is allowed");
  if (!/^(select|with)\b/i.test(s)) throw new Error("only read-only SELECT/WITH queries are allowed");
  if (BLOCK.test(s)) throw new Error("query uses a disallowed keyword (read-only, no filesystem/DDL/DML)");
  return s;
}
function presentTables(id) {
  const dir = indexDir(id);
  return TABLES.filter((t) => existsSync6(join4(dir, `${t}.parquet`)));
}
async function setupViews(conn, id) {
  const dir = indexDir(id);
  const present = /* @__PURE__ */ new Set();
  for (const t of TABLES) {
    const p = join4(dir, `${t}.parquet`);
    if (existsSync6(p)) {
      await conn.run(`CREATE VIEW ${t} AS SELECT * FROM '${p.replace(/'/g, "''")}'`);
      present.add(t);
    }
  }
  if (present.has("commits") && present.has("blame")) {
    for (const stmt of VIEWS_L1) await conn.run(stmt);
    if (present.has("symbols") && present.has("symbol_refs"))
      for (const stmt of VIEWS_L3) await conn.run(stmt);
    if (present.has("semantic_refs"))
      for (const stmt of VIEWS_SEMANTIC) await conn.run(stmt);
  }
  if (present.has("dependencies")) for (const stmt of VIEWS_DEPS) await conn.run(stmt);
  if (present.has("insights")) for (const stmt of VIEWS_RISKS) await conn.run(stmt);
  if (present.has("code_markers") && present.has("files"))
    for (const stmt of VIEWS_MARKERS) await conn.run(stmt);
}
function withConn2(id, fn) {
  return withConn(id, (c) => setupViews(c, id), fn);
}
var jsonSafe = (rows) => JSON.parse(JSON.stringify(rows, (_k, v) => typeof v === "bigint" ? Number(v) : v));
function planEstimate(planText) {
  const rows = [...planText.matchAll(/~\s*([\d]+)\s*Rows/g)].map((m) => Number(m[1]));
  const sumEst = rows.reduce((a, b) => a + b, 0);
  const sorted = [...rows].sort((a, b) => b - a);
  const blowup = /CROSS_PRODUCT|NESTED_LOOP|IE[_ ]?JOIN|BLOCKWISE_NL/i.test(planText);
  const crossEst = blowup && sorted.length >= 2 ? sorted[0] * sorted[1] : 0;
  const estimated_cost_rows = Math.max(sumEst, crossEst);
  const estimated_result_rows = rows.length ? rows[0] : 0;
  return { estimated_cost_rows, estimated_result_rows, cross_product: blowup, over_budget: estimated_cost_rows > COST_ROW_BUDGET, budget: COST_ROW_BUDGET };
}
async function estimate(id, sql, withPlan = false) {
  const safe = validate(sql);
  const wrapped = `WITH __q AS (${safe}) SELECT * FROM __q LIMIT ${ROW_CAP + 1}`;
  return withConn2(id, async (conn) => {
    const rows = await (await conn.run(`EXPLAIN ${wrapped}`)).getRowObjects();
    const plan = rows.map((r) => String(r.explain_value ?? Object.values(r).join(" "))).join("\n");
    const est = planEstimate(plan);
    return withPlan ? { ...est, plan } : est;
  });
}
async function runQuery(id, sql) {
  const est = await estimate(id, sql);
  if (est.over_budget)
    throw new Error(`estimated cost ${est.estimated_cost_rows} rows exceeds the ${COST_ROW_BUDGET}-row budget \u2014 narrow the query (add WHERE / LIMIT / a smaller join)`);
  const wrapped = `WITH __q AS (${validate(sql)}) SELECT * FROM __q LIMIT ${ROW_CAP + 1}`;
  return withConn2(id, async (conn) => {
    const t0 = Date.now();
    const rows = await Promise.race([
      conn.run(wrapped).then((r) => r.getRowObjects()),
      new Promise((_, rej) => setTimeout(() => rej(new Error(`query exceeded ${TIMEOUT_MS}ms cap`)), TIMEOUT_MS))
    ]);
    const ms = Date.now() - t0;
    recordQuery(id, ms);
    const truncated = rows.length > ROW_CAP;
    return { rows: jsonSafe(truncated ? rows.slice(0, ROW_CAP) : rows), row_count: Math.min(rows.length, ROW_CAP), truncated, elapsed_ms: ms, estimate: est };
  });
}
async function runPinned(id, pin2) {
  const est = await estimate(id, pin2.sql);
  if (est.estimated_cost_rows > pin2.cost_ceiling)
    throw new Error(`pinned query '${pin2.name}' drifted: estimated ${est.estimated_cost_rows} rows > ceiling ${pin2.cost_ceiling} \u2014 re-pin it`);
  const wrapped = `WITH __q AS (${validate(pin2.sql)}) SELECT * FROM __q LIMIT ${ROW_CAP + 1}`;
  return withConn2(id, async (conn) => {
    const t0 = Date.now();
    const rows = await Promise.race([
      conn.run(wrapped).then((r) => r.getRowObjects()),
      new Promise((_, rej) => setTimeout(() => rej(new Error(`pinned query '${pin2.name}' exceeded its ${pin2.slo_ms}ms SLO`)), pin2.slo_ms))
    ]);
    const ms = Date.now() - t0;
    recordQuery(id, ms);
    const truncated = rows.length > ROW_CAP;
    return {
      pin: pin2.name,
      rows: jsonSafe(truncated ? rows.slice(0, ROW_CAP) : rows),
      row_count: Math.min(rows.length, ROW_CAP),
      truncated,
      elapsed_ms: ms,
      cost_rows: est.estimated_cost_rows,
      cost_ceiling: pin2.cost_ceiling,
      slo_ms: pin2.slo_ms
    };
  });
}
function send(res, code, body) {
  const s = JSON.stringify(body, null, 2);
  res.writeHead(code, { "content-type": "application/json" });
  res.end(s);
}
function serve(port) {
  const server = createServer(async (req, res) => {
    const reqId = newRequestId();
    const t0 = Date.now();
    const path = (req.url ?? "/").split("?")[0];
    res.on("finish", () => recordRequest({ reqId, method: req.method, path, status: res.statusCode, ms: Date.now() - t0 }));
    try {
      const url = new URL(req.url ?? "/", "http://localhost");
      const parts = url.pathname.split("/").filter(Boolean);
      if (url.pathname === "/health") return send(res, 200, { ok: true });
      if (url.pathname === "/metrics" && req.method === "GET") return send(res, 200, snapshot());
      if (url.pathname === "/webhook" && req.method === "POST") {
        let body = "";
        for await (const chunk of req) body += chunk;
        let payload = {};
        try {
          payload = JSON.parse(body || "{}");
        } catch {
        }
        const repo = payload.repository;
        const t = payload.tenant ? findTenant(String(payload.tenant)) : repo && (tenantForRepo(repo.clone_url ?? "") ?? tenantForRepo(repo.html_url ?? ""));
        if (!t) return send(res, 400, { error: "no tenant matched (send {tenant} or a payload with repository.clone_url)" });
        setImmediate(() => {
          try {
            sync(t.id);
          } catch (e) {
            console.error(`webhook sync ${t.id} failed:`, e.message);
          }
        });
        return send(res, 202, { accepted: true, tenant: t.id });
      }
      if (url.pathname === "/") {
        return send(res, 200, {
          public_index: loadTenants().filter((t) => t.visibility === "public").map((t) => ({ id: t.id, name: t.name })),
          usage: "GET /t/<tenant>/context (list surfaces); GET /t/<tenant>/context/<surface>?path=&days=&limit= (curated answer); POST /t/<tenant>/query (SQL body); POST /t/<tenant>/explain (pre-execution cost estimate); GET /t/<tenant>/tables; GET /t/<tenant>/meters"
        });
      }
      if (parts[0] === "t" && parts[1]) {
        const id = parts[1];
        const t = findTenant(id);
        const write2 = parts[2] === "pins" && (req.method === "POST" && !parts[3] || req.method === "DELETE" && !!parts[3]);
        let keyId = null;
        try {
          keyId = authorize(req.headers, t, { write: write2 }).keyId;
        } catch (e) {
          if (e instanceof AuthError) return send(res, e.status, { error: e.message });
          throw e;
        }
        const rl = take(keyId ?? `anon:${id}`);
        if (!rl.ok) {
          res.setHeader("retry-after", String(Math.ceil((rl.retry_after_ms ?? 1e3) / 1e3)));
          return send(res, 429, { error: "rate limit exceeded", retry_after_ms: rl.retry_after_ms });
        }
        if (parts[2] === "tables" && req.method === "GET") {
          const present = presentTables(id);
          const views = [
            ...VIEW_NAMES_L1,
            ...present.includes("symbols") && present.includes("symbol_refs") ? VIEW_NAMES_L3 : [],
            ...present.includes("semantic_refs") ? VIEW_NAMES_SEMANTIC : [],
            ...present.includes("dependencies") ? VIEW_NAMES_DEPS : [],
            ...present.includes("insights") ? VIEW_NAMES_RISKS : [],
            ...present.includes("code_markers") ? VIEW_NAMES_MARKERS : []
          ];
          return send(res, 200, { tenant: id, views, tables: present });
        }
        if (parts[2] === "meters" && req.method === "GET") return send(res, 200, read(id));
        if (parts[2] === "explain" && req.method === "POST") {
          let body = "";
          for await (const chunk of req) body += chunk;
          const sql = body.trim();
          if (!sql) return send(res, 400, { error: "empty SQL body" });
          return send(res, 200, { tenant: id, ...await estimate(id, sql, true) });
        }
        if (parts[2] === "query" && req.method === "POST") {
          let body = "";
          for await (const chunk of req) body += chunk;
          const sql = body.trim();
          if (!sql) return send(res, 400, { error: "empty SQL body" });
          const result = await runQuery(id, sql);
          return send(res, 200, { tenant: id, visibility: t.visibility, ...result });
        }
        if (parts[2] === "context" && req.method === "GET") {
          const name = parts[3];
          if (!name) return send(res, 200, { tenant: id, surfaces: catalogue() });
          const surface = SURFACES[name];
          if (!surface)
            return send(res, 404, { error: `no context surface '${name}'`, available: Object.keys(SURFACES) });
          const params2 = parseParams(url.searchParams);
          let sql;
          try {
            sql = surface.build(params2);
          } catch (e) {
            return send(res, 400, { error: e.message });
          }
          const result = await runQuery(id, sql);
          return send(res, 200, { tenant: id, surface: name, params: params2, visibility: t.visibility, ...result });
        }
        if (parts[2] === "pins") {
          const name = parts[3];
          if (!name && req.method === "GET") return send(res, 200, { tenant: id, pins: list(id) });
          if (!name && req.method === "POST") {
            let body = "";
            for await (const chunk of req) body += chunk;
            let p = {};
            try {
              p = JSON.parse(body || "{}");
            } catch {
              return send(res, 400, { error: "body must be JSON {name, sql, cost_ceiling?, slo_ms?}" });
            }
            if (!p.name || !p.sql) return send(res, 400, { error: "name and sql are required" });
            const est = await estimate(id, String(p.sql));
            const rec = pin(
              id,
              String(p.name),
              String(p.sql),
              est.estimated_cost_rows,
              { cost_ceiling: p.cost_ceiling, slo_ms: p.slo_ms },
              Math.floor(Date.now() / 1e3)
            );
            return send(res, 201, { tenant: id, ...rec });
          }
          if (name && parts[4] === "run" && req.method === "POST") {
            const pin2 = get(id, name);
            if (!pin2) return send(res, 404, { error: `no pinned query '${name}'` });
            return send(res, 200, { tenant: id, visibility: t.visibility, ...await runPinned(id, pin2) });
          }
          if (name && req.method === "DELETE")
            return send(res, remove(id, name) ? 200 : 404, { tenant: id, removed: name });
        }
      }
      send(res, 404, { error: "not found" });
    } catch (e) {
      reportError(e, { req_id: reqId, path });
      send(res, 400, { error: e.message });
    }
  });
  server.listen(port, () => log("info", "listening", { port, endpoint: `http://localhost:${port}` }));
}

// src/evals.ts
import { execFileSync } from "child_process";
import { existsSync as existsSync7, readFileSync as readFileSync6 } from "fs";
import { join as join5 } from "path";
var TABLES2 = ["commits", "commit_parents", "authors", "files", "commit_files", "blame", "identities", "identity_aliases", "identity_reviews", "symbols", "symbol_refs", "semantic_refs"];
var readSql2 = (name) => readFileSync6(new URL(`../${name}`, import.meta.url), "utf8").split(";").map((s) => s.split("\n").filter((l) => !l.trim().startsWith("--")).join("\n").trim()).filter(Boolean);
async function setupViews2(conn, id) {
  const dir = indexDir(id);
  const present = /* @__PURE__ */ new Set();
  for (const t of TABLES2) {
    const p = join5(dir, `${t}.parquet`);
    if (existsSync7(p)) {
      await conn.run(`CREATE VIEW ${t} AS SELECT * FROM '${p.replace(/'/g, "''")}'`);
      present.add(t);
    }
  }
  if (present.has("commits") && present.has("blame")) {
    for (const s of readSql2("views.sql")) await conn.run(s);
    if (present.has("symbols") && present.has("symbol_refs")) for (const s of readSql2("views_l3.sql")) await conn.run(s);
    if (present.has("semantic_refs")) for (const s of readSql2("views_semantic.sql")) await conn.run(s);
  }
}
var num = (v) => typeof v === "bigint" ? Number(v) : v;
var params = (p) => ({ days: 3650, limit: 50, ...p });
function makeGit(repo) {
  return (args) => execFileSync("git", ["-C", repo, ...args], { encoding: "utf8", maxBuffer: 256 * 1024 * 1024 }).trim();
}
async function runEvals(tenantId) {
  const t = findTenant(tenantId);
  const repo = cloneSource(t);
  const manifest = JSON.parse(readFileSync6(join5(indexDir(tenantId), "manifest.json"), "utf8"));
  const head = manifest.head_sha;
  const git = makeGit(repo);
  const q = (sql) => withConn(tenantId, (c) => setupViews2(c, tenantId), async (c) => (await c.run(sql)).getRowObjects());
  const results = [];
  const idMap = /* @__PURE__ */ new Map();
  for (const r of await q(
    `SELECT a.email AS raw, COALESCE(NULLIF(i.email, ''), a.email) AS canon
     FROM authors a LEFT JOIN identities i ON a.identity_id = i.identity_id`
  ))
    idMap.set((r.raw ?? "").toLowerCase(), r.canon);
  const canon = (email) => idMap.get((email ?? "").toLowerCase()) ?? email ?? "";
  const gitBlameTop = (f) => {
    const counts = /* @__PURE__ */ new Map();
    for (const line of git(["blame", "--line-porcelain", head, "--", f]).split("\n")) {
      if (line.startsWith("author-mail ")) {
        const email = line.slice("author-mail ".length).replace(/[<>]/g, "").trim();
        counts.set(email, (counts.get(email) ?? 0) + 1);
      }
    }
    return [...counts.entries()].sort((a, b) => b[1] - a[1])[0]?.[0];
  };
  const ownedFiles = (await q(
    `SELECT path FROM v_blame GROUP BY path HAVING count(*) BETWEEN 40 AND 4000 ORDER BY count(*) DESC LIMIT 15`
  )).map((r) => r.path);
  {
    let match = 0;
    for (const f of ownedFiles) {
      const top = (await q(SURFACES.ownership.build(params({ path: f, limit: 1 }))))[0]?.author_email;
      if (top && top === canon(gitBlameTop(f))) match++;
    }
    const metric = ownedFiles.length ? match / ownedFiles.length : 1;
    results.push({ name: "ownership.top_owner", metric, threshold: 0.9, pass: metric >= 0.9, detail: `${match}/${ownedFiles.length} files' top owner == git blame (identity-resolved)` });
  }
  {
    let match = 0;
    for (const f of ownedFiles) {
      const top = (await q(SURFACES.reviewers.build(params({ path: f, limit: 1 }))))[0]?.author_email;
      if (top && top === canon(gitBlameTop(f))) match++;
    }
    const metric = ownedFiles.length ? match / ownedFiles.length : 1;
    results.push({ name: "reviewers.top_is_owner", metric, threshold: 0.9, pass: metric >= 0.9, detail: `${match}/${ownedFiles.length} files' top reviewer == git-blame owner` });
  }
  {
    const defs = await q(
      `SELECT name, path FROM v_symbols WHERE length(name) >= 5 GROUP BY name, path ORDER BY name LIMIT 12`
    );
    let found = 0;
    for (const d of defs) {
      const hits = await q(SURFACES.search.build(params({ query: d.name, limit: 25 })));
      if (hits.some((h) => h.type === "symbol" && h.path === d.path && h.title === d.name)) found++;
    }
    const metric = defs.length ? found / defs.length : 1;
    results.push({ name: "search.symbol_recall", metric, threshold: 0.9, pass: metric >= 0.9, detail: `${found}/${defs.length} sampled symbols found at their file` });
  }
  {
    const top = (await q(SURFACES.contributors.build(params({ limit: 1 }))))[0]?.author_email;
    const gitCounts = /* @__PURE__ */ new Map();
    for (const e of git(["log", "--no-merges", "--format=%ae", head, "--", "."]).split("\n").filter(Boolean))
      gitCounts.set(e, (gitCounts.get(e) ?? 0) + 1);
    const gitTop = canon([...gitCounts.entries()].sort((a, b) => b[1] - a[1])[0]?.[0]);
    const ok = !!top && top === gitTop;
    results.push({ name: "contributors.top_author", metric: ok ? 1 : 0, threshold: 1, pass: ok, detail: `surface #1 ${top} vs git #1 ${gitTop} (identity-resolved)` });
  }
  {
    const [a] = await q(SURFACES.activity.build(params({ days: 3650 })));
    const gitAuthors = new Set(git(["log", "--no-merges", "--format=%ae", head, "--", "."]).split("\n").filter(Boolean).map(canon)).size;
    const ok = num(a.authors) === gitAuthors;
    results.push({ name: "activity.authors", metric: ok ? 1 : 0, threshold: 1, pass: ok, detail: `authors ${num(a.authors)} vs git ${gitAuthors} (identity-resolved)` });
  }
  {
    let hits = 0, total = 0;
    for (const f of ownedFiles.slice(0, 5)) {
      const surf = (await q(SURFACES.blast_radius.build(params({ path: f, limit: 5 })))).map((r) => r.path);
      const shas = git(["log", "--no-merges", "--format=%H", head, "--", f]).split("\n").filter(Boolean);
      const coChanged = /* @__PURE__ */ new Set();
      for (const sha of shas)
        for (const p of git(["show", "--no-merges", "--name-only", "--format=", sha]).split("\n").filter(Boolean)) coChanged.add(p);
      for (const p of surf) {
        total++;
        if (coChanged.has(p)) hits++;
      }
    }
    const metric = total ? hits / total : 1;
    results.push({ name: "blast_radius.real_coupling", metric, threshold: 0.9, pass: metric >= 0.9, detail: `${hits}/${total} top coupled files are real git co-changes` });
  }
  return results;
}
async function evalCli(tenantId) {
  const results = await runEvals(tenantId);
  console.log(`
Answer-quality evals for '${tenantId}' (surfaces vs git):
`);
  for (const r of results) {
    const pct = (r.metric * 100).toFixed(1).padStart(5);
    console.log(`  ${r.pass ? "\u2713" : "\u2717"} ${r.name.padEnd(28)} ${pct}%  (>=${(r.threshold * 100).toFixed(0)}%)  ${r.detail}`);
  }
  const failed = results.filter((r) => !r.pass);
  console.log(`
${results.length - failed.length}/${results.length} passed.${failed.length ? " FAILED: " + failed.map((r) => r.name).join(", ") : ""}
`);
  if (failed.length) process.exit(1);
}

// src/cli.ts
function main() {
  const [cmd, ...rest] = process.argv.slice(2);
  const portOf = (def) => {
    const i = rest.indexOf("--port");
    return i >= 0 ? Number(rest[i + 1]) : def;
  };
  switch (cmd) {
    case "local": {
      const args = rest.filter((a) => !a.startsWith("--"));
      const repo = resolve2(args[0] ?? ".");
      const port = portOf(8787);
      process.env.EOS_LOCAL_REPO = repo;
      ensureDataDirs();
      const out = indexDir("local");
      mkdirSync3(out, { recursive: true });
      console.log(`\u25B6 indexing ${repo} \u2192 local index \u2026`);
      const r = spawnSync2(GITINDEX, ["index", repo, "--out", out], { stdio: "inherit" });
      if (r.status !== 0) {
        console.error(`
index failed (is the engine built? \u2014 'cargo build --release' in engine/)`);
        process.exit(1);
      }
      const cfg = {
        mcpServers: {
          "eos-index": { command: "eos-mcp", env: { EOS_API_URL: `http://localhost:${port}`, EOS_TENANT: "local" } }
        }
      };
      console.log(`
\u2713 indexed. serving at http://localhost:${port}/t/local
`);
      console.log(`  Point an agent at it \u2014 add this to your MCP config (no API key needed):
`);
      console.log(JSON.stringify(cfg, null, 2).split("\n").map((l) => "    " + l).join("\n"));
      console.log(`
  Or query directly, e.g.:`);
      console.log(`    curl -s "localhost:${port}/t/local/context/ownership?path=src"`);
      console.log(`    curl -s "localhost:${port}/t/local/context/search?query=<term>"
`);
      serve(port);
      return;
    }
    case "serve": {
      serve(portOf(8787));
      return;
    }
    case "eval": {
      const tenant = rest[0] ?? "local";
      evalCli(tenant).catch((e) => {
        console.error(e);
        process.exit(1);
      });
      return;
    }
    case "list":
      for (const t of loadTenants())
        console.log(`  ${t.visibility === "public" ? "\u{1F310}" : "\u{1F512}"} ${t.id.padEnd(14)} ${t.name} (${t.url ?? t.path})`);
      return;
    default:
      console.log("usage: eos-index <local|serve|eval|list> [args]");
      console.log("  eos-index local <repo>   # index a repo + serve it locally (no account)");
  }
}
main();
