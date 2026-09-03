import { z } from "zod";
import { defaultClient, EOSClient } from "../lib/client.js";

export const getActivitySchema = z.object({
  path: z.string().optional().describe("Optional file/directory to scope activity to."),
  days: z.coerce.number().int().min(1).max(3650).optional().describe("History window in days (1–3650, default 90)."),
});

export type GetActivityInput = z.infer<typeof getActivitySchema>;

interface ActivityRow {
  commits:            number;
  file_changes:       number;
  files:              number;
  authors:            number;
  lines_added:        number;
  lines_removed:      number;
  window_start_epoch: number | null;
  window_end_epoch:   number | null;
}

const day = (e: number | null) => (e ? new Date(e * 1000).toISOString().slice(0, 10) : "—");

export async function getActivityHandler(
  input: GetActivityInput,
  client: EOSClient = defaultClient,
): Promise<string> {
  const { path, days } = input;
  const { rows } = await client.getSurface<ActivityRow>("activity", { path, days });
  const a = rows[0];

  if (!a || !a.commits) {
    return `No activity${path ? ` for \`${path}\`` : ""} in the last ${days ?? 90} days of history.`;
  }

  const scope = path ? `\`${path}\`` : "the codebase";
  return [
    `## Activity in ${scope} — window ${day(a.window_start_epoch)} → ${day(a.window_end_epoch)}`,
    "",
    `- **Commits:** ${a.commits.toLocaleString()}`,
    `- **File changes:** ${a.file_changes.toLocaleString()} across ${a.files.toLocaleString()} file${a.files !== 1 ? "s" : ""}`,
    `- **Authors:** ${a.authors}`,
    `- **Lines:** +${a.lines_added.toLocaleString()} / −${a.lines_removed.toLocaleString()}`,
  ].join("\n");
}
