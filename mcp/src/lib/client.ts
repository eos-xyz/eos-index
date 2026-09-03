/**
 * Typed HTTP client for the EOS git-native index (the `eng/hosted` service).
 *
 * Talks to the per-tenant context surfaces served over the git index:
 *   GET {EOS_API_URL}/t/{EOS_TENANT}/context/<surface>?path=&days=&limit=
 *
 * Env:
 *   EOS_API_URL  base URL of the eos-hosted service (default http://localhost:8787)
 *   EOS_TENANT   the tenant / index id to query
 *   EOS_API_KEY  bearer key (omit for a public tenant)
 *
 * All errors surface as EOSAPIError with a clear, status-mapped message so tool
 * handlers can show them to the LLM.
 */

export const DEFAULT_BASE_URL = "http://localhost:8787";

export class EOSAPIError extends Error {
  constructor(
    public readonly status: number,
    message: string,
  ) {
    super(message);
    this.name = "EOSAPIError";
  }
}

function translateStatus(status: number, body: string): string {
  switch (status) {
    case 401: return "Invalid API key. Verify EOS_API_KEY is set correctly in your MCP config.";
    case 403: return `Not authorized for this tenant. Check EOS_TENANT and your key's scope. (${body})`;
    case 404: return "Not found — check EOS_TENANT (the index id) and the surface name.";
    case 429: return "Rate limit hit — slow down and retry in a moment.";
    default:  return status >= 500
      ? `EOS hosted service error (HTTP ${status}). Try again later.`
      : `EOS error (HTTP ${status}): ${body}`;
  }
}

/** The envelope every context surface returns (see eng/hosted/src/surfaces.ts). */
export interface SurfaceResult<T> {
  tenant: string;
  surface: string;
  params: Record<string, unknown>;
  rows: T[];
  row_count: number;
  truncated: boolean;
  elapsed_ms: number;
}

function qs(params: Record<string, string | number | undefined>): string {
  const p = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) if (v !== undefined && v !== "") p.set(k, String(v));
  const s = p.toString();
  return s ? `?${s}` : "";
}

export function createClient(opts?: { apiKey?: string; baseUrl?: string; tenant?: string }) {
  // Config is read lazily (per request), not captured at creation, so a local
  // bootstrap that sets EOS_API_URL / EOS_TENANT *after* module load (see
  // local.ts) still takes effect. Explicit opts always win (used by tests).
  const cfg = () => ({
    apiKey:  opts?.apiKey  ?? process.env.EOS_API_KEY,
    baseUrl: (opts?.baseUrl ?? process.env.EOS_API_URL ?? DEFAULT_BASE_URL).replace(/\/$/, ""),
    tenant:  opts?.tenant  ?? process.env.EOS_TENANT,
  });

  async function request<T>(path: string, init?: RequestInit): Promise<T> {
    const { apiKey, baseUrl } = cfg();
    const headers: Record<string, string> = { "Content-Type": "application/json", ...(init?.headers as Record<string, string> ?? {}) };
    // A public tenant needs no key; only send one if we have it.
    if (apiKey) headers["Authorization"] = `Bearer ${apiKey}`;

    const res = await fetch(`${baseUrl}${path}`, { ...init, headers });

    if (!res.ok) {
      const body = await res.text().catch(() => "");
      throw new EOSAPIError(res.status, translateStatus(res.status, body));
    }
    return res.json() as Promise<T>;
  }

  return {
    get:  <T>(path: string)                => request<T>(path),
    post: <T>(path: string, body: unknown) => request<T>(path, { method: "POST", body: JSON.stringify(body) }),
    /** Query a curated context surface for the configured tenant. */
    getSurface: async <T>(surface: string, params: Record<string, string | number | undefined>) => {
      const { tenant } = cfg();
      if (!tenant)
        throw new EOSAPIError(400, "EOS_TENANT is not set — set it to the index/tenant id you want to query in your MCP config.");
      return request<SurfaceResult<T>>(`/t/${encodeURIComponent(tenant)}/context/${surface}${qs(params)}`);
    },
  };
}

export type EOSClient = ReturnType<typeof createClient>;

// Singleton used by tool handlers at runtime (reads env at call time, not module load).
export const defaultClient: EOSClient = createClient();
