import { z } from "zod";
import { defaultClient, EOSClient } from "../lib/client.js";

export const getMarkersSchema = z.object({
  path:  z.string().optional().describe("Optional file/directory to scope to."),
  limit: z.coerce.number().int().min(1).max(500).optional().describe("Max markers to return (1–500, default 50)."),
});

export type GetMarkersInput = z.infer<typeof getMarkersSchema>;

interface MarkerRow {
  path:   string;
  line:   number;
  marker: string;
  text:   string;
}

export async function getMarkersHandler(
  input: GetMarkersInput,
  client: EOSClient = defaultClient,
): Promise<string> {
  const { path, limit } = input;
  const { rows } = await client.getSurface<MarkerRow>("markers", { path, limit });

  if (!rows.length) return `No technical-debt markers${path ? ` in \`${path}\`` : ""}.`;

  const lines = [`## Technical-debt markers${path ? ` in \`${path}\`` : ""} (${rows.length})`, ""];
  for (const m of rows) {
    lines.push(`- **${m.marker}** ${m.path}:${m.line}${m.text ? ` — ${m.text}` : ""}`);
  }
  return lines.join("\n");
}
