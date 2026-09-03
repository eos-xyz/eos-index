import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { getOwnershipHandler }     from "../src/tools/get-ownership.js";
import { getCouplingHandler }      from "../src/tools/get-coupling.js";
import { getContributorsHandler }  from "../src/tools/get-contributors.js";
import { suggestReviewersHandler } from "../src/tools/suggest-reviewers.js";
import { getActivityHandler }      from "../src/tools/get-activity.js";
import { searchCodeHandler }       from "../src/tools/search-code.js";
import { getGraphHandler }         from "../src/tools/get-graph.js";
import { getDependenciesHandler }  from "../src/tools/get-dependencies.js";
import { getRisksHandler }         from "../src/tools/get-risks.js";
import { getMarkersHandler }       from "../src/tools/get-markers.js";
import { createClient } from "../src/lib/client.js";

const mockFetch = vi.fn();
beforeEach(() => { vi.stubGlobal("fetch", mockFetch); mockFetch.mockReset(); });
afterEach(() => vi.unstubAllGlobals());

const client = createClient({ apiKey: "eos_test", baseUrl: "https://test.eos.dev", tenant: "acme" });
// Every surface returns the same envelope shape (see eng/hosted/src/server.ts).
const ok = (rows: unknown[]) =>
  mockFetch.mockResolvedValue({ ok: true, json: async () => ({ tenant: "acme", surface: "s", params: {}, rows, row_count: rows.length, truncated: false, elapsed_ms: 1 }) });

const EPOCH = 1_788_000_000; // 2026-09-…

describe("get_ownership", () => {
  it("lists owners with share, flags bus-factor, and builds the surface URL", async () => {
    ok([{ author_name: "Alice", author_email: "a@x.dev", owned_lines: 900, files: 5, share: 0.9, last_touch_epoch: EPOCH }]);
    const out = await getOwnershipHandler({ path: "src/auth", limit: 5 }, client);
    const [url] = mockFetch.mock.calls[0] as [string];
    expect(url).toContain("/t/acme/context/ownership?");
    expect(url).toContain("path=src%2Fauth");
    expect(url).toContain("limit=5");
    expect(out).toContain("Alice");
    expect(out).toContain("90%");
    expect(out).toContain("Bus-factor risk");
  });

  it("returns a friendly message when there is no ownership data", async () => {
    ok([]);
    const out = await getOwnershipHandler({ path: "src/x" }, client);
    expect(out).toContain("No ownership data");
  });
});

describe("suggest_reviewers", () => {
  it("ranks reviewers with evidence and marks the top pick", async () => {
    ok([
      { author_name: "Carla", author_email: "c@x.dev", owned_lines: 500, recent_changes: 8, last_touch_epoch: EPOCH },
      { author_name: "Ed",    author_email: "e@x.dev", owned_lines: 10,  recent_changes: 1, last_touch_epoch: EPOCH },
    ]);
    const out = await suggestReviewersHandler({ path: "src/auth" }, client);
    const [url] = mockFetch.mock.calls[0] as [string];
    expect(url).toContain("/t/acme/context/reviewers?");
    expect(url).toContain("path=src%2Fauth");
    expect(out).toContain("Carla");
    expect(out).toContain("top pick");
    expect(out).toContain("owns 500 lines here");
  });
});

describe("get_coupling (blast radius)", () => {
  it("lists coupled files and forwards params", async () => {
    ok([{ path: "b.ts", co_changes: 7, last_co_change_epoch: EPOCH }]);
    const out = await getCouplingHandler({ path: "a.ts", limit: 20 }, client);
    const [url] = mockFetch.mock.calls[0] as [string];
    expect(url).toContain("/t/acme/context/blast_radius?");
    expect(url).toContain("path=a.ts");
    expect(url).toContain("limit=20");
    expect(out).toContain("`b.ts`");
    expect(out).toContain("7×");
  });
});

describe("get_activity", () => {
  it("summarises the window and forwards days", async () => {
    ok([{ commits: 40, file_changes: 100, files: 30, authors: 3, lines_added: 500, lines_removed: 120, window_start_epoch: EPOCH - 30 * 86400, window_end_epoch: EPOCH }]);
    const out = await getActivityHandler({ days: 30 }, client);
    const [url] = mockFetch.mock.calls[0] as [string];
    expect(url).toContain("/t/acme/context/activity?");
    expect(url).toContain("days=30");
    expect(out).toContain("Commits:** 40");
    expect(out).toContain("Authors:** 3");
  });

  it("reports quiet windows", async () => {
    ok([{ commits: 0, file_changes: 0, files: 0, authors: 0, lines_added: 0, lines_removed: 0, window_start_epoch: null, window_end_epoch: null }]);
    const out = await getActivityHandler({ days: 7 }, client);
    expect(out).toContain("No activity");
  });
});

describe("get_contributors", () => {
  it("renders the roster and forwards limit", async () => {
    ok([{ author_name: "Bob", author_email: "b@x.dev", commits: 10, file_changes: 40, files: 12, first_seen_epoch: EPOCH - 100 * 86400, last_seen_epoch: EPOCH }]);
    const out = await getContributorsHandler({ limit: 20 }, client);
    const [url] = mockFetch.mock.calls[0] as [string];
    expect(url).toContain("/t/acme/context/contributors?");
    expect(url).toContain("limit=20");
    expect(out).toContain("Contributors (1)");
    expect(out).toContain("Bob");
  });
});

describe("search_code", () => {
  it("groups symbols/paths/commits, cites them, and forwards the query", async () => {
    ok([
      { type: "symbol", title: "compute", path: "src/deps.rs", line: 50, sha: null, detail: "function", rank: 3 },
      { type: "path",   title: "src/deps.rs", path: "src/deps.rs", line: null, sha: null, detail: null, rank: 1 },
      { type: "commit", title: "feat: deps layer", path: null, line: null, sha: "abcdef1234", detail: "Isra", rank: 1 },
    ]);
    const out = await searchCodeHandler({ query: "compute", limit: 20 }, client);
    const [url] = mockFetch.mock.calls[0] as [string];
    expect(url).toContain("/t/acme/context/search?");
    expect(url).toContain("query=compute");
    expect(out).toContain("**Definitions**");
    expect(out).toContain("`compute`");
    expect(out).toContain("src/deps.rs:50");
    expect(out).toContain("abcdef12");
  });

  it("reports no matches", async () => {
    ok([]);
    const out = await searchCodeHandler({ query: "zzz" }, client);
    expect(out).toContain("No matches");
  });
});

describe("get_graph", () => {
  it("lists dependency edges and forwards path scope", async () => {
    ok([{ from_path: "a.rs", to_path: "b.rs", refs: 85 }]);
    const out = await getGraphHandler({ path: "a.rs", limit: 30 }, client);
    const [url] = mockFetch.mock.calls[0] as [string];
    expect(url).toContain("/t/acme/context/graph?");
    expect(url).toContain("path=a.rs");
    expect(out).toContain("`a.rs` → `b.rs`");
    expect(out).toContain("85 refs");
  });
});

describe("get_dependencies", () => {
  it("lists deps and forwards the query filter", async () => {
    ok([{ ecosystem: "npm", name: "@duckdb/node-api", version: "^1.2.0", scope: "runtime", manifest_path: "eng/bench/package.json" }]);
    const out = await getDependenciesHandler({ query: "npm", limit: 5 }, client);
    const [url] = mockFetch.mock.calls[0] as [string];
    expect(url).toContain("/t/acme/context/dependencies?");
    expect(url).toContain("query=npm");
    expect(out).toContain("@duckdb/node-api");
    expect(out).toContain("eng/bench/package.json");
  });
});

describe("get_risks", () => {
  it("lists findings with severity and forwards path scope", async () => {
    ok([{ kind: "bus_factor_risk", severity: "critical", subject: "src/auth.ts", metric: 1, detail: "single owner owns 100%" }]);
    const out = await getRisksHandler({ path: "src", limit: 10 }, client);
    const [url] = mockFetch.mock.calls[0] as [string];
    expect(url).toContain("/t/acme/context/risks?");
    expect(url).toContain("path=src");
    expect(out).toContain("bus_factor_risk");
    expect(out).toContain("single owner");
  });
});

describe("get_markers", () => {
  it("lists markers with file:line", async () => {
    ok([{ path: "src/a.ts", line: 42, marker: "TODO", text: "handle nulls" }]);
    const out = await getMarkersHandler({ limit: 20 }, client);
    const [url] = mockFetch.mock.calls[0] as [string];
    expect(url).toContain("/t/acme/context/markers?");
    expect(out).toContain("TODO");
    expect(out).toContain("src/a.ts:42");
  });
});
