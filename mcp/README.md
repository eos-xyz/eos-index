# @eos-ai/mcp-server

Connect the EOS **git-native code index** to Claude Desktop, Cursor, Windsurf,
and any [Model Context Protocol](https://modelcontextprotocol.io)-compatible IDE
— so your agent can ground its work in a repository's real history: who owns a
file, who should review a change to it, what else moves when you touch it, what's
active, and who contributes. Every answer is derived from git and carries dated
evidence.

The server talks to the **eos-hosted** service (`eng/hosted`), which serves each
tenant's git index. Point it at a hosted instance (or your own — local mode is
the same engine).

## Quick start

**1. Point at an index**

You need three things: the hosted service URL (`EOS_API_URL`), the tenant/index
id (`EOS_TENANT`), and — for a private tenant — an API key (`EOS_API_KEY`). A
public tenant needs no key.

**2. Install the server**

```bash
npm install -g @eos-ai/mcp-server
```

**3. Add to your IDE config** (see per-IDE instructions below)

---

## Claude Desktop

Edit `~/Library/Application Support/Claude/claude_desktop_config.json`
(macOS) or `%APPDATA%\Claude\claude_desktop_config.json` (Windows):

```json
{
  "mcpServers": {
    "eos": {
      "command": "eos-mcp",
      "env": {
        "EOS_API_URL": "http://localhost:8787",
        "EOS_TENANT": "your-index-id",
        "EOS_API_KEY": "eos_your_key_here"
      }
    }
  }
}
```

Restart Claude Desktop. You'll see the EOS tools available in the tool picker.

---

## Cursor

Open **Cursor Settings** → **MCP** → **Add new global MCP server**, then paste:

```json
{
  "eos": {
    "command": "eos-mcp",
    "env": {
      "EOS_API_URL": "http://localhost:8787",
      "EOS_TENANT": "your-index-id",
      "EOS_API_KEY": "eos_your_key_here"
    }
  }
}
```

Or edit `~/.cursor/mcp.json` directly with the same object.

---

## Windsurf

Edit `~/.codeium/windsurf/mcp_config.json`:

```json
{
  "mcpServers": {
    "eos": {
      "command": "eos-mcp",
      "env": {
        "EOS_API_URL": "http://localhost:8787",
        "EOS_TENANT": "your-index-id",
        "EOS_API_KEY": "eos_your_key_here"
      }
    }
  }
}
```

---

## Environment

| Variable | Required | Description |
|---|---|---|
| `EOS_API_URL` | recommended | Base URL of the eos-hosted service. Defaults to `http://localhost:8787`. |
| `EOS_TENANT`  | yes | The tenant / index id to query. |
| `EOS_API_KEY` | for private tenants | Bearer key. Omit for a public tenant. |

---

## Local mode (self-contained, no account)

Set **`EOS_REPO`** and the MCP boots a local `eos-index` server over that repo
itself (index + serve) and points at it — one config entry, no cloud, no key:

```json
{
  "mcpServers": {
    "eos-index": {
      "command": "eos-mcp",
      "env": { "EOS_REPO": "/path/to/your/repo" }
    }
  }
}
```

`eos-index` must be on `PATH` (or set `EOS_INDEX_BIN` to the built bundle;
`EOS_LOCAL_PORT` overrides the default `8787`). Without `EOS_REPO`, the server
runs in remote mode against `EOS_API_URL` / `EOS_TENANT` (below).

## Available tools

Each tool answers a question over the tenant's git index. Every answer is
git-native and carries dated evidence.

| Tool | Description |
|------|-------------|
| `get_ownership` | Owners/experts for a file or directory by blame lines, with each owner's share and a bus-factor flag when one person dominates. |
| `suggest_reviewers` | Reviewers for a change to a path — the people who most own it and have most recently changed it, ranked, with evidence. |
| `get_coupling` | The blast radius of a path — files that historically change together with it (temporal coupling), even without an import edge. |
| `get_activity` | Change volume over the last N days of history (commits, files, authors, lines), optionally scoped to a path. |
| `get_contributors` | Contributors with their footprint — commits, files, active span — optionally scoped to a path/area. |
| `search_code` | Structural search over symbol definitions, file paths, and commit subjects for a term — ranked and cited (path:line or commit sha). Exact, no embeddings. |
| `get_graph` | The resolved file-to-file dependency graph — who imports/uses whom, from the L3 reference graph — optionally scoped to a path. |
| `get_dependencies` | External dependencies declared in HEAD manifests (ecosystem/version/scope); optional query filter. |
| `get_risks` | Precomputed code-intelligence findings — bus-factor, hotspots, hidden coupling, fragile/architecture hubs — optionally scoped to a path. |
| `get_markers` | Technical-debt markers (TODO/FIXME/…) in HEAD, with file:line; optionally scoped to a path. |

### Examples

```
"Who owns eng/gitindex/src/ingest.rs?"
→ uses get_ownership { path: "eng/gitindex/src/ingest.rs" }

"Who should review a change to src/lib/auth?"
→ uses suggest_reviewers { path: "src/lib/auth" }

"What breaks if I touch this file?"
→ uses get_coupling { path: "src/lib/auth.ts" }

"What's been changing in the last month?"
→ uses get_activity { days: 30 }

"Who works on the payments area?"
→ uses get_contributors { path: "src/payments" }

"Where is refreshToken defined?"
→ uses search_code { query: "refreshToken" }

"What does src/lib/auth.ts depend on?"
→ uses get_graph { path: "src/lib/auth.ts" }
```

---

## Development

```bash
# From repo root
npm install --workspace=packages/mcp-server

# Build
npm run build --workspace=@eos-ai/mcp-server

# Test
npm run test --workspace=@eos-ai/mcp-server

# Typecheck
npm run typecheck --workspace=@eos-ai/mcp-server
```

---

## Publishing

```bash
npm run build --workspace=@eos-ai/mcp-server
cd packages/mcp-server
npm publish --access public
```

Requires npm credentials and `@eos` org access. The `dist/` directory is
included in the published package; `src/` and `__tests__/` are not.
