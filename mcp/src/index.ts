/**
 * EOS MCP Server — git-native code context
 *
 * Exposes the EOS git index (the `eng/hosted` service) as MCP tools so any
 * MCP-compatible agent (Claude Desktop, Cursor, Windsurf, etc.) can ground its
 * work in a repository's real history: who owns a file, who should review a
 * change to it, what else moves when you touch it, what's active, and who
 * contributes. Every answer is derived from git and carries dated evidence.
 *
 * Transport: stdio (default for local IDE use)
 * Env:       EOS_API_URL (eos-hosted base, default http://localhost:8787),
 *            EOS_TENANT (the index/tenant id), EOS_API_KEY (bearer; omit for a
 *            public tenant)
 */

import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";

import { getOwnershipSchema, getOwnershipHandler, type GetOwnershipInput }             from "./tools/get-ownership.js";
import { getCouplingSchema, getCouplingHandler, type GetCouplingInput }                from "./tools/get-coupling.js";
import { getContributorsSchema, getContributorsHandler, type GetContributorsInput }    from "./tools/get-contributors.js";
import { suggestReviewersSchema, suggestReviewersHandler, type SuggestReviewersInput } from "./tools/suggest-reviewers.js";
import { getActivitySchema, getActivityHandler, type GetActivityInput }                from "./tools/get-activity.js";
import { searchCodeSchema, searchCodeHandler, type SearchCodeInput }                   from "./tools/search-code.js";
import { getGraphSchema, getGraphHandler, type GetGraphInput }                         from "./tools/get-graph.js";
import { getDependenciesSchema, getDependenciesHandler, type GetDependenciesInput }    from "./tools/get-dependencies.js";
import { getRisksSchema, getRisksHandler, type GetRisksInput }                         from "./tools/get-risks.js";
import { getMarkersSchema, getMarkersHandler, type GetMarkersInput }                   from "./tools/get-markers.js";
import { EOSAPIError } from "./lib/client.js";
import { bootLocalIfRequested } from "./lib/local.js";

// ─── Server ───────────────────────────────────────────────────────────────────

export function createServer(): McpServer {
  const server = new McpServer({
    name:    "eos",
    version: "2.0.0",
  });

  // ── get_ownership ─────────────────────────────────────────────────────────────
  server.tool("get_ownership",
    "Get the owners/experts for a file or directory — who to ask before touching " +
    "it — by blame lines, with each owner's share and a bus-factor flag when one " +
    "person dominates. " +
    "Example: get_ownership { path: 'src/lib/auth' }.",
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    getOwnershipSchema as any,
    async (input: GetOwnershipInput) => {
      const text = await wrapError(() => getOwnershipHandler(input));
      return { content: [{ type: "text" as const, text }] };
    },
  );

  // ── suggest_reviewers ────────────────────────────────────────────────────────
  server.tool("suggest_reviewers",
    "Suggest reviewers for a change to a given path — the people who most own it " +
    "(blame) and have most recently changed it, ranked, with the evidence for each. " +
    "Example: suggest_reviewers { path: 'src/lib/auth' }.",
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    suggestReviewersSchema as any,
    async (input: SuggestReviewersInput) => {
      const text = await wrapError(() => suggestReviewersHandler(input));
      return { content: [{ type: "text" as const, text }] };
    },
  );

  // ── get_coupling (blast radius) ───────────────────────────────────────────────
  server.tool("get_coupling",
    "Get the blast radius of a path — the files that have historically changed " +
    "together with it in the same commit (temporal coupling). The hidden-dependency " +
    "signal an import graph can't see: edit one, check the others. " +
    "Example: get_coupling { path: 'src/lib/auth.ts' }.",
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    getCouplingSchema as any,
    async (input: GetCouplingInput) => {
      const text = await wrapError(() => getCouplingHandler(input));
      return { content: [{ type: "text" as const, text }] };
    },
  );

  // ── get_activity ─────────────────────────────────────────────────────────────
  server.tool("get_activity",
    "Get change volume over the last N days of history — commits, files, authors, " +
    "and lines added/removed — optionally scoped to a path. Anchored to the newest " +
    "commit in the index, so it's honest even if the index lags.",
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    getActivitySchema as any,
    async (input: GetActivityInput) => {
      const text = await wrapError(() => getActivityHandler(input));
      return { content: [{ type: "text" as const, text }] };
    },
  );

  // ── get_contributors ─────────────────────────────────────────────────────────
  server.tool("get_contributors",
    "List contributors with their footprint — commits, files touched, and active " +
    "span — so you know who to route work or questions to. Pass a `path` to scope " +
    "it to who works on a file or area.",
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    getContributorsSchema as any,
    async (input: GetContributorsInput) => {
      const text = await wrapError(() => getContributorsHandler(input));
      return { content: [{ type: "text" as const, text }] };
    },
  );

  // ── search_code ──────────────────────────────────────────────────────────────
  server.tool("search_code",
    "Structural search across the index — symbol definitions, file paths, and " +
    "commit subjects — for a term, ranked and cited (path:line or commit sha). " +
    "Exact, no embeddings. Reach for this to find where something is defined, which " +
    "file holds it, or the commit that introduced it. " +
    "Examples: search_code { query: 'refreshToken' }, search_code { query: 'auth' }.",
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    searchCodeSchema as any,
    async (input: SearchCodeInput) => {
      const text = await wrapError(() => searchCodeHandler(input));
      return { content: [{ type: "text" as const, text }] };
    },
  );

  // ── get_graph ────────────────────────────────────────────────────────────────
  server.tool("get_graph",
    "Get the resolved file-to-file dependency graph — who imports/uses whom, from " +
    "the L3 reference graph — ranked by how many references cross each edge. Pass a " +
    "`path` to scope it to edges touching a file or area.",
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    getGraphSchema as any,
    async (input: GetGraphInput) => {
      const text = await wrapError(() => getGraphHandler(input));
      return { content: [{ type: "text" as const, text }] };
    },
  );

  // ── get_dependencies ─────────────────────────────────────────────────────────
  server.tool("get_dependencies",
    "List the external dependencies declared in the repo's HEAD manifests " +
    "(ecosystem, version, scope). Pass `query` to filter by package name or " +
    "ecosystem (npm/cargo/pypi/go).",
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    getDependenciesSchema as any,
    async (input: GetDependenciesInput) => {
      const text = await wrapError(() => getDependenciesHandler(input));
      return { content: [{ type: "text" as const, text }] };
    },
  );

  // ── get_risks ────────────────────────────────────────────────────────────────
  server.tool("get_risks",
    "Get precomputed code-intelligence findings — bus-factor risk, hotspots, hidden " +
    "coupling, fragile/architecture hubs — ranked by severity. Pass a `path` to " +
    "scope to a file or area. Reach for this to know what's risky before you touch it.",
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    getRisksSchema as any,
    async (input: GetRisksInput) => {
      const text = await wrapError(() => getRisksHandler(input));
      return { content: [{ type: "text" as const, text }] };
    },
  );

  // ── get_markers ──────────────────────────────────────────────────────────────
  server.tool("get_markers",
    "List technical-debt markers (TODO/FIXME/HACK/XXX/NOTE) in the repo's HEAD, with " +
    "file:line. Pass a `path` to scope to a file or area.",
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    getMarkersSchema as any,
    async (input: GetMarkersInput) => {
      const text = await wrapError(() => getMarkersHandler(input));
      return { content: [{ type: "text" as const, text }] };
    },
  );

  return server;
}

// ─── Error wrapper ────────────────────────────────────────────────────────────

async function wrapError(fn: () => Promise<string>): Promise<string> {
  try {
    return await fn();
  } catch (err) {
    if (err instanceof EOSAPIError) return `**EOS Error:** ${err.message}`;
    const msg = err instanceof Error ? err.message : String(err);
    return `**Unexpected error:** ${msg}`;
  }
}

// ─── Entry point ─────────────────────────────────────────────────────────────

export async function main(): Promise<void> {
  // Self-contained local mode: if EOS_REPO is set, boot a local eos-index server
  // over that repo and point the client at it (logs to stderr so stdio stays clean).
  await bootLocalIfRequested((s) => process.stderr.write(`[eos-mcp] ${s}\n`));
  const server    = createServer();
  const transport = new StdioServerTransport();
  await server.connect(transport);
}
