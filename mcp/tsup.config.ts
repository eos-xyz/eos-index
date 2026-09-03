import { defineConfig } from "tsup";

export default defineConfig([
  // Library entry — compiled to dist/src/index.js
  {
    entry: { "src/index": "src/index.ts" },
    format: ["cjs"],
    target: "node20",
    platform: "node",
    outDir: "dist",
    clean: true,
    dts: false,
  },
  // CLI binary — compiled to dist/bin/eos-mcp.js with shebang
  {
    entry: { "bin/eos-mcp": "bin/eos-mcp.ts" },
    format: ["cjs"],
    target: "node20",
    platform: "node",
    outDir: "dist",
    dts: false,
    banner: {
      js: "#!/usr/bin/env node",
    },
  },
]);
