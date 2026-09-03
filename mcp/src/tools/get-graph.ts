import { z } from "zod";
import { defaultClient, EOSClient } from "../lib/client.js";

export const getGraphSchema = z.object({
  path:  z.string().optional().describe("Optional file/directory to scope the graph to edges touching it."),
  limit: z.coerce.number().int().min(1).max(200).optional().describe("Max edges to return (1–200, default 30)."),
});

export type GetGraphInput = z.infer<typeof getGraphSchema>;

interface Edge {
  from_path: string;
  to_path:   string;
  refs:      number;
}

export async function getGraphHandler(
  input: GetGraphInput,
  client: EOSClient = defaultClient,
): Promise<string> {
  const { path, limit } = input;
  const { rows } = await client.getSurface<Edge>("graph", { path, limit });

  if (!rows.length) return `No resolved dependency edges${path ? ` touching \`${path}\`` : ""} yet.`;

  const scope = path ? ` around \`${path}\`` : "";
  const lines = [`## Dependency graph${scope} — ${rows.length} edge${rows.length !== 1 ? "s" : ""}`, ""];
  for (const e of rows) {
    lines.push(`- \`${e.from_path}\` → \`${e.to_path}\` (${e.refs} ref${e.refs !== 1 ? "s" : ""})`);
  }
  lines.push("", "_Resolved file-to-file references — who imports/uses whom, from the L3 reference graph._");
  return lines.join("\n");
}
