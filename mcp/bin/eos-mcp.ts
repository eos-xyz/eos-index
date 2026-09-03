import { main } from "../src/index.js";

main().catch((err) => {
  console.error("[eos-mcp] fatal:", err);
  process.exit(1);
});
