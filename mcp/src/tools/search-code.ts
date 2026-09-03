import { z } from "zod";
import { defaultClient, EOSClient } from "../lib/client.js";

export const searchCodeSchema = z.object({
  query: z.string().describe("Search term — a symbol name, path fragment, or word from a commit message."),
  limit: z.coerce.number().int().min(1).max(100).optional().describe("Max results to return (1–100, default 20)."),
});

export type SearchCodeInput = z.infer<typeof searchCodeSchema>;

interface Hit {
  type:   "symbol" | "path" | "commit";
  title:  string;
  path:   string | null;
  line:   number | null;
  sha:    string | null;
  detail: string | null;
  rank:   number;
}

export async function searchCodeHandler(
  input: SearchCodeInput,
  client: EOSClient = defaultClient,
): Promise<string> {
  const { query, limit } = input;
  const { rows } = await client.getSurface<Hit>("search", { query, limit });

  if (!rows.length) return `No matches for \`${query}\` in symbols, paths, or commit subjects.`;

  const symbols = rows.filter((r) => r.type === "symbol");
  const paths   = rows.filter((r) => r.type === "path");
  const commits = rows.filter((r) => r.type === "commit");
  const out: string[] = [`## Results for \`${query}\``];

  if (symbols.length) {
    out.push("", "**Definitions**");
    for (const s of symbols) out.push(`- \`${s.title}\`${s.detail ? ` (${s.detail})` : ""} — ${s.path}:${s.line}`);
  }
  if (paths.length) {
    out.push("", "**Files**");
    for (const p of paths) out.push(`- \`${p.path}\``);
  }
  if (commits.length) {
    out.push("", "**Commits**");
    for (const c of commits) out.push(`- ${c.title}${c.detail ? ` — ${c.detail}` : ""} (\`${(c.sha ?? "").slice(0, 8)}\`)`);
  }
  return out.join("\n");
}
