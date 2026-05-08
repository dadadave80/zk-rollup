/**
 * Static-only Bun.serve entry for the web UI.
 *
 *   bun run web
 *
 * Serves web/index.html (and bundles the imported app.tsx + styles.css on the
 * fly via Bun's HTML imports). The page talks to the sequencer at
 * http://localhost:7001 directly — sequencer has permissive CORS so this works
 * across origins. Run `bun run demo` (or demo:groth16/sepolia) first, in a
 * separate terminal, to bring the rollup services up.
 */

import index from "./web/index.html";

const PORT = Number(process.env.WEB_PORT ?? 3000);

Bun.serve({
  port: PORT,
  routes: {
    "/": index,
  },
  development: {
    hmr: true,
    console: true,
  },
});

console.log(`web UI on http://localhost:${PORT}`);
console.log("(make sure 'bun run demo' is up in another terminal — it owns sequencer/prover-svc/anvil)");
