import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { createClient } from "../src/lib/client.js";

const mockFetch = vi.fn();

beforeEach(() => {
  vi.stubGlobal("fetch", mockFetch);
  mockFetch.mockReset();
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("createClient", () => {
  it("sends the Authorization header when a key is set", async () => {
    mockFetch.mockResolvedValue({ ok: true, json: async () => ({ ok: true }) });

    const client = createClient({ apiKey: "eos_test_key", baseUrl: "https://api.test" });
    await client.get("/t/acme/context/ownership");

    expect(mockFetch).toHaveBeenCalledOnce();
    const [url, init] = mockFetch.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("https://api.test/t/acme/context/ownership");
    expect((init.headers as Record<string, string>)["Authorization"]).toBe("Bearer eos_test_key");
  });

  it("omits Authorization for a public tenant (no key)", async () => {
    mockFetch.mockResolvedValue({ ok: true, json: async () => ({ ok: true }) });

    const client = createClient({ apiKey: undefined, baseUrl: "https://api.test" });
    await client.get("/t/pub/context/activity");

    const [, init] = mockFetch.mock.calls[0] as [string, RequestInit];
    expect((init.headers as Record<string, string>)["Authorization"]).toBeUndefined();
  });

  it("getSurface builds the per-tenant surface URL with params", async () => {
    mockFetch.mockResolvedValue({ ok: true, json: async () => ({ rows: [] }) });

    const client = createClient({ apiKey: "k", baseUrl: "https://api.test", tenant: "acme" });
    await client.getSurface("ownership", { path: "src/auth", limit: 5 });

    const [url] = mockFetch.mock.calls[0] as [string];
    expect(url).toContain("https://api.test/t/acme/context/ownership?");
    expect(url).toContain("path=src%2Fauth");
    expect(url).toContain("limit=5");
  });

  it("reads EOS_API_URL / EOS_TENANT lazily (per request, for local boot)", async () => {
    mockFetch.mockResolvedValue({ ok: true, json: async () => ({ rows: [] }) });
    const client = createClient(); // no opts → env at call time
    const prevUrl = process.env.EOS_API_URL, prevTenant = process.env.EOS_TENANT;
    process.env.EOS_API_URL = "https://env.test";
    process.env.EOS_TENANT = "envtenant";
    try {
      await client.getSurface("ownership", { path: "x" });
      const [url] = mockFetch.mock.calls[0] as [string];
      expect(url).toBe("https://env.test/t/envtenant/context/ownership?path=x");
    } finally {
      if (prevUrl === undefined) delete process.env.EOS_API_URL; else process.env.EOS_API_URL = prevUrl;
      if (prevTenant === undefined) delete process.env.EOS_TENANT; else process.env.EOS_TENANT = prevTenant;
    }
  });

  it("getSurface throws when EOS_TENANT is not set", async () => {
    const client = createClient({ apiKey: "k", baseUrl: "https://api.test", tenant: undefined });
    await expect(client.getSurface("ownership", { path: "x" })).rejects.toThrow("EOS_TENANT is not set");
  });

  it("translates 401 to a clear message", async () => {
    mockFetch.mockResolvedValue({ ok: false, status: 401, text: async () => "" });

    const client = createClient({ apiKey: "eos_bad_key", baseUrl: "https://api.test" });
    await expect(client.get("/t/acme/context/x")).rejects.toThrow("Invalid API key");
  });

  it("translates 429 to a rate-limit message", async () => {
    mockFetch.mockResolvedValue({ ok: false, status: 429, text: async () => "" });

    const client = createClient({ apiKey: "eos_key", baseUrl: "https://api.test" });
    await expect(client.get("/t/acme/context/x")).rejects.toThrow("Rate limit hit");
  });

  it("sends POST body as JSON", async () => {
    mockFetch.mockResolvedValue({ ok: true, json: async () => ({ ok: true }) });

    const client = createClient({ apiKey: "eos_key", baseUrl: "https://api.test" });
    await client.post("/t/acme/query", { sql: "select 1" });

    const [, init] = mockFetch.mock.calls[0] as [string, RequestInit];
    expect(init.method).toBe("POST");
    expect(init.body).toBe(JSON.stringify({ sql: "select 1" }));
  });
});
