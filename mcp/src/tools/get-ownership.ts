import { z } from "zod";
import { defaultClient, EOSClient } from "../lib/client.js";

export const getOwnershipSchema = z.object({
  path:  z.string().describe("File path or directory to attribute ownership for, e.g. 'src/lib/auth'."),
  limit: z.coerce.number().int().min(1).max(50).optional().describe("Max owners to return (1–50, default 10)."),
});

export type GetOwnershipInput = z.infer<typeof getOwnershipSchema>;

interface OwnerRow {
  author_name:      string;
  author_email:     string;
  owned_lines:      number;
  files:            number;
  share:            number;
  last_touch_epoch: number;
}

const day = (e: number | null) => (e ? new Date(e * 1000).toISOString().slice(0, 10) : "—");

export async function getOwnershipHandler(
  input: GetOwnershipInput,
  client: EOSClient = defaultClient,
): Promise<string> {
  const { path, limit } = input;
  const { rows } = await client.getSurface<OwnerRow>("ownership", { path, limit });

  if (!rows.length) return `No ownership data for \`${path}\` yet.`;

  const lines = [`## Owners of \`${path}\` (by blame lines)`, ""];
  for (const o of rows) {
    lines.push(
      `- **${o.author_name}** — ${o.owned_lines.toLocaleString()} line${o.owned_lines !== 1 ? "s" : ""} ` +
      `(${Math.round(o.share * 100)}%), ${o.files} file${o.files !== 1 ? "s" : ""}, last touched ${day(o.last_touch_epoch)}`,
    );
  }
  const top = rows[0];
  lines.push("");
  if (top.share >= 0.75) {
    lines.push(`> ⚠️ **Bus-factor risk:** ${top.author_name} owns ${Math.round(top.share * 100)}% of this area — consider a second reviewer / knowledge share.`);
  } else {
    lines.push(`> Knowledge is spread — top owner ${top.author_name} holds ${Math.round(top.share * 100)}%.`);
  }
  return lines.join("\n");
}
