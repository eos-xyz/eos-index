import { z } from "zod";
import { defaultClient, EOSClient } from "../lib/client.js";

export const getDependenciesSchema = z.object({
  query: z.string().optional().describe("Optional filter — a package name substring or an exact ecosystem (npm/cargo/pypi/go)."),
  limit: z.coerce.number().int().min(1).max(500).optional().describe("Max dependencies to return (1–500, default 50)."),
});

export type GetDependenciesInput = z.infer<typeof getDependenciesSchema>;

interface DepRow {
  ecosystem:     string;
  name:          string;
  version:       string;
  scope:         string;
  manifest_path: string;
}

export async function getDependenciesHandler(
  input: GetDependenciesInput,
  client: EOSClient = defaultClient,
): Promise<string> {
  const { query, limit } = input;
  const { rows } = await client.getSurface<DepRow>("dependencies", { query, limit });

  if (!rows.length) return `No dependencies${query ? ` matching \`${query}\`` : ""} found in HEAD manifests.`;

  const lines = [`## Dependencies${query ? ` matching \`${query}\`` : ""} (${rows.length})`, ""];
  for (const d of rows) {
    lines.push(`- **${d.name}** \`${d.version || "—"}\` · ${d.ecosystem}/${d.scope} · ${d.manifest_path}`);
  }
  return lines.join("\n");
}
