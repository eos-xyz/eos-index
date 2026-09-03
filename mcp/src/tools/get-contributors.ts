import { z } from "zod";
import { defaultClient, EOSClient } from "../lib/client.js";

export const getContributorsSchema = z.object({
  path:  z.string().optional().describe("Optional file/directory to scope the roster to (who works on this area)."),
  limit: z.coerce.number().int().min(1).max(500).optional().describe("Max contributors to return (1–500, default 20)."),
});

export type GetContributorsInput = z.infer<typeof getContributorsSchema>;

interface ContributorRow {
  author_name:      string;
  author_email:     string;
  commits:          number;
  file_changes:     number;
  files:            number;
  first_seen_epoch: number;
  last_seen_epoch:  number;
}

const day = (e: number | null) => (e ? new Date(e * 1000).toISOString().slice(0, 10) : "—");

export async function getContributorsHandler(
  input: GetContributorsInput,
  client: EOSClient = defaultClient,
): Promise<string> {
  const { path, limit } = input;
  const { rows } = await client.getSurface<ContributorRow>("contributors", { path, limit });

  if (!rows.length) return `No contributors${path ? ` for \`${path}\`` : ""} yet.`;

  const scope = path ? ` in \`${path}\`` : "";
  const lines = [
    `## Contributors${scope} (${rows.length})`,
    "",
    "| Contributor | Commits | Files | Active span |",
    "|---|---|---|---|",
  ];
  for (const c of rows) {
    lines.push(`| ${c.author_name} | ${c.commits.toLocaleString()} | ${c.files.toLocaleString()} | ${day(c.first_seen_epoch)} → ${day(c.last_seen_epoch)} |`);
  }
  return lines.join("\n");
}
