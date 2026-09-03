import { z } from "zod";
import { defaultClient, EOSClient } from "../lib/client.js";

export const getRisksSchema = z.object({
  path:  z.string().optional().describe("Optional path/area to scope findings to (matches the finding's subject)."),
  limit: z.coerce.number().int().min(1).max(200).optional().describe("Max findings to return (1–200, default 20)."),
});

export type GetRisksInput = z.infer<typeof getRisksSchema>;

interface RiskRow {
  kind:     string;
  severity: string;
  subject:  string;
  metric:   number;
  detail:   string;
}

const ICON: Record<string, string> = { critical: "🔴", warning: "🟠", info: "•" };

export async function getRisksHandler(
  input: GetRisksInput,
  client: EOSClient = defaultClient,
): Promise<string> {
  const { path, limit } = input;
  const { rows } = await client.getSurface<RiskRow>("risks", { path, limit });

  if (!rows.length) return `No code-intelligence findings${path ? ` for \`${path}\`` : ""}.`;

  const lines = [`## Code risks${path ? ` for \`${path}\`` : ""} (${rows.length})`, ""];
  for (const r of rows) {
    lines.push(`- ${ICON[r.severity] ?? "•"} **${r.kind}** — ${r.detail || r.subject}`);
  }
  lines.push("", "_Precomputed from git history: bus-factor, hotspots, hidden coupling, fragile/architecture hubs._");
  return lines.join("\n");
}
