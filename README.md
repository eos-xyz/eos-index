# eos-index

**Local, git-native code context for your AI agent.** Point it at a git
repository and it builds a deep, queryable index of the repo's real history —
ownership, reviewers, blast radius, dependencies, risks, symbols, and more — then
serves it to Claude, Gemini, Cursor, or any MCP-compatible agent. Runs entirely
on your machine: **no account, no cloud, no API key.**

> This directory is the source of the standalone open-source `eos-index` tool
> (Apache-2.0). It is assembled from the monorepo by `assemble.sh` and published
> to its own public repository. See "Layout" below.

## Quick start

```bash
# 1. get the engine (prebuilt binary from Releases, or `cargo build --release`)
# 2. index a repo and serve it locally
eos-index local /path/to/your/repo
```

That indexes the repo and serves it at `http://localhost:8787/t/local`, then
prints an MCP config to paste into your agent. Or drive it yourself:

```bash
curl -s "localhost:8787/t/local/context/ownership?path=src/lib/auth"
curl -s "localhost:8787/t/local/context/search?query=refreshToken"
curl -s "localhost:8787/t/local/context/blast_radius?path=src/lib/auth.ts"
```

## With an AI agent (one config entry)

Set `EOS_REPO` and the MCP server boots the local index itself — the agent needs
nothing else:

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

Then ask, over your real git history: *who owns this file, who should review a
change to it, what breaks if I touch it, where is this defined, what's risky
here, what does it depend on.*

## What you get

| Surface / tool | Answers |
|---|---|
| `ownership` | who owns a file/area (by blame), with a bus-factor flag |
| `reviewers` | who should review a change to a path |
| `blast_radius` | what historically changes together with a path |
| `search` | symbol / path / commit search, cited to `path:line` |
| `graph` | resolved file→file dependency graph |
| `dependencies` | external deps from the manifests |
| `risks` | bus-factor, hotspots, hidden coupling, fragile hubs |
| `markers` | TODO/FIXME/… with file:line |
| `activity`, `contributors` | change volume and per-person footprint |

Every answer is derived from git and verified against it. Authorship is
identity-resolved (a person's aliases count once).

## Local vs hosted

`eos-index` is the **local, open** path — you run it, you own the index. The
managed, always-fresh, multi-tenant hosted service (and org-wide analytics) is a
separate offering; this tool needs none of it.

## Layout (when assembled)

- `engine/` — the Rust indexer (`gitindex`)
- `server/` — the local server + curated surfaces (`eng/hosted`)
- `mcp/` — the MCP server (`@eos-ai/mcp-server`)

Run `./assemble.sh` to produce the standalone repo tree under `build/`.

## License

Apache-2.0 — see [LICENSE](./LICENSE).
