import { z } from "zod";
import { defaultClient, EOSClient } from "../lib/client.js";

export const getCouplingSchema = z.object({
  path:  z.string().describe("File or directory to compute the blast radius for, e.g. 'src/lib/auth.ts'."),
  limit: z.coerce.number().int().min(1).max(100).optional().describe("Max coupled files to return (1–100, default 20)."),
});

export type GetCouplingInput = z.infer<typeof getCouplingSchema>;

interface CoupledRow {
  path:                 string;
  co_changes:           number;
  last_co_change_epoch: number;
}

const day = (e: number | null) => (e ? new Date(e * 1000).toISOString().slice(0, 10) : "—");

export async function getCouplingHandler(
  input: GetCouplingInput,
  client: EOSClient = defaultClient,
): Promise<string> {
  const { path, limit } = input;
  const { rows } = await client.getSurface<CoupledRow>("blast_radius", { path, limit });

  if (!rows.length) return `No temporal coupling for \`${path}\` yet — not enough shared-change history.`;

  const lines = [`## Blast radius of \`${path}\` — ${rows.length} file${rows.length !== 1 ? "s" : ""} that tend to change with it`, ""];
  for (const r of rows) {
    lines.push(`- \`${r.path}\` — changed together ${r.co_changes}× (last ${day(r.last_co_change_epoch)})`);
  }
  lines.push("", "_These move with the target even without an import edge — edit it, check these._");
  return lines.join("\n");
}
