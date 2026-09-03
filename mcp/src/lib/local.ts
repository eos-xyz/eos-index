/**
 * Local mode — make the MCP server self-contained.
 *
 * When EOS_REPO is set, the MCP boots a local `eos-index` server over that repo
 * itself (index + serve, no account), waits until it answers, and points the
 * client at it. So an agent needs only ONE config entry — no separate serve
 * step, no cloud, no key:
 *
 *   { "command": "eos-mcp", "env": { "EOS_REPO": "/path/to/repo" } }
 *
 * Env:
 *   EOS_REPO        the repo to index + serve locally (enables local mode)
 *   EOS_INDEX_BIN   path to the eos-index bundle (dist/hosted.js); if unset,
 *                   falls back to an `eos-index` executable on PATH
 *   EOS_LOCAL_PORT  port for the local server (default 8787)
 */

import { spawn, type ChildProcess } from "node:child_process";

let child: ChildProcess | undefined;

async function waitForHealth(base: string, timeoutMs = 120_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const r = await fetch(`${base}/health`);
      if (r.ok) return;
    } catch {
      /* not up yet */
    }
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error(`local eos-index did not become healthy within ${timeoutMs}ms`);
}

/**
 * If EOS_REPO is set, spawn a local eos-index server over it and point the
 * client (EOS_API_URL / EOS_TENANT) at it. Idempotent-ish: returns immediately
 * when EOS_REPO is not set (remote mode). Resolves once the server is healthy.
 */
export async function bootLocalIfRequested(log: (s: string) => void = () => {}): Promise<void> {
  const repo = process.env.EOS_REPO;
  if (!repo) return; // remote mode — nothing to boot

  const port = process.env.EOS_LOCAL_PORT ?? "8787";
  const base = `http://localhost:${port}`;
  const bin = process.env.EOS_INDEX_BIN;
  // `node <bundle>` when EOS_INDEX_BIN points at the dist bundle; otherwise the
  // `eos-index` executable on PATH (the published/installed tool).
  const [cmd, pre] = bin ? ["node", [bin]] : ["eos-index", [] as string[]];
  const args = [...pre, "local", repo, "--port", String(port)];

  log(`booting local eos-index over ${repo} on ${base} …`);
  child = spawn(cmd, args, { stdio: ["ignore", "inherit", "inherit"] });
  child.on("exit", (code) => {
    if (code && code !== 0) log(`local eos-index exited with code ${code}`);
  });
  // Tear the child down with us.
  const kill = () => { try { child?.kill(); } catch { /* ignore */ } };
  process.on("exit", kill);
  process.on("SIGINT", () => { kill(); process.exit(0); });
  process.on("SIGTERM", () => { kill(); process.exit(0); });

  await waitForHealth(base);
  process.env.EOS_API_URL = base;
  process.env.EOS_TENANT = "local";
  log(`local eos-index ready at ${base}/t/local`);
}
