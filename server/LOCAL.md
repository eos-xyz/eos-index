# Run EOS locally (phase 1)

Index a git repository on your own machine and let an AI agent (Claude, Gemini,
Cursor, …) query it — **no account, no cloud, no API key.** This is the
open-core, local-first path: you download the engine and run it yourself.

## One command

```bash
# 1. build the engine once
cargo build --release --manifest-path ../gitindex/Cargo.toml

# 2. index a repo and serve it locally
node --experimental-strip-types src/hosted.ts local /path/to/your/repo
```

That indexes the repo into a local index and serves it at
`http://localhost:8787/t/local`. It then prints an MCP config you can paste into
your agent.

## Point an agent at it

Add the printed config to your MCP client (Claude Desktop / Cursor / Windsurf).
No key is needed — the local tenant is served openly on your machine:

```json
{
  "mcpServers": {
    "eos-index": {
      "command": "eos-mcp",
      "env": { "EOS_API_URL": "http://localhost:8787", "EOS_TENANT": "local" }
    }
  }
}
```

Now your agent can ask, over your real git history:

- who owns / should review a file (`get_ownership`, `suggest_reviewers`)
- what breaks if you touch it (`get_coupling` — blast radius)
- where a symbol is defined (`search_code`)
- what's active, who contributes (`get_activity`, `get_contributors`)

## Or query it directly

```bash
curl -s "localhost:8787/t/local/context/ownership?path=src/lib/auth"
curl -s "localhost:8787/t/local/context/search?query=refreshToken"
curl -s "localhost:8787/t/local/context/blast_radius?path=src/lib/auth.ts"
# raw SQL over the index, too:
curl -s -X POST "localhost:8787/t/local/query" --data "select repo, count(*) from commits"
```

## What this is (and isn't)

- **Local & open** — everything runs on your machine, over your repo, with no
  EOS account. The engine is the value; you own the index.
- **Not the hosted product** — the managed, always-fresh, multi-tenant service
  (and the org-wide ) is the enterprise path; this is the local tool.

Re-run `hosted local <repo>` after new commits to refresh the index (it's
incremental — only the delta is processed).
