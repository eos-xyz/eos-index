import { z } from "zod";
import { defaultClient, EOSClient } from "../lib/client.js";

export const suggestReviewersSchema = z.object({
  path:  z.string().describe("File or directory the change touches, e.g. 'src/lib/auth'."),
  limit: z.coerce.number().int().min(1).max(50).optional().describe("Max reviewers to return (1–50, default 10)."),
});

export type SuggestReviewersInput = z.infer<typeof suggestReviewersSchema>;

interface ReviewerRow {
  author_name:      string;
  author_email:     string;
  owned_lines:      number;
  recent_changes:   number;
  last_touch_epoch: number | null;
}

const day = (e: number | null) => (e ? new Date(e * 1000).toISOString().slice(0, 10) : "—");

export async function suggestReviewersHandler(
  input: SuggestReviewersInput,
  client: EOSClient = defaultClient,
): Promise<string> {
  const { path, limit } = input;
  const { rows } = await client.getSurface<ReviewerRow>("reviewers", { path, limit });

  if (!rows.length) return `No reviewer signal for \`${path}\` yet.`;

  const lines = [`## Suggested reviewers for a change to \`${path}\``, ""];
  rows.forEach((r, i) => {
    const evidence = [
      r.owned_lines ? `owns ${r.owned_lines.toLocaleString()} lines here` : null,
      r.recent_changes ? `${r.recent_changes} recent change${r.recent_changes !== 1 ? "s" : ""}` : null,
      r.last_touch_epoch ? `last touched ${day(r.last_touch_epoch)}` : null,
    ].filter(Boolean).join(", ");
    lines.push(`- **${r.author_name}**${i === 0 ? " ⭐ **top pick**" : ""}\n  ${evidence || "prior activity in this area"}`);
  });
  lines.push("", "_Ranked by ownership of this path and recent changes to it — the people who know this code._");
  return lines.join("\n");
}
