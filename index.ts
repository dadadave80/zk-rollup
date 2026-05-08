/**
 * One-shot orchestrator for the SP1 zk-rollup demo.
 *
 *   bun run demo            # mock-prover end-to-end on a local anvil
 *   bun run demo:groth16    # real Groth16 proving on a local anvil
 *   bun run demo:sepolia    # real Groth16 proving on Sepolia (needs env)
 *
 * The script owns the full lifecycle of L1 + prover-svc + sequencer, so it
 * always boots from a clean known state. Press Ctrl-C to tear everything down.
 */

import { spawn, type Subprocess } from "bun";
import { existsSync } from "node:fs";
import { resolve, join } from "node:path";
import { privateKeyToAccount } from "viem/accounts";
import { concat, hexToBytes, keccak256, numberToBytes, type Hex } from "viem";

const REPO = resolve(import.meta.dirname);
const TARGET_RELEASE = join(REPO, "target", "release");
const CONTRACTS = join(REPO, "contracts");
const GENESIS_PATH = join(REPO, "genesis.json");

const NETWORK = (process.env.NETWORK ?? "anvil").toLowerCase();
const IS_SEPOLIA = NETWORK === "sepolia";
const PROOF_MODE = process.env.PROOF_MODE ?? (IS_SEPOLIA ? "groth16" : "mock");

const PROVER_PORT = 7002;
const SEQUENCER_PORT = 7001;
const ANVIL_PORT = 8545;

// Canonical SP1 verifier gateway on Sepolia (per Succinct's docs).
// The gateway only dispatches to verifiers whose selectors are registered
// upstream; if your SP1 SDK version isn't registered yet, set
// SP1_VERIFIER to this address explicitly only when you've confirmed it
// supports your proof's selector.
// eslint-disable-next-line @typescript-eslint/no-unused-vars
const _SEPOLIA_SP1_GATEWAY = "0x3B6041173B80E77f038f3F2C0f9744f04837185e" as Hex;

const ALICE_KEY = "0x0101010101010101010101010101010101010101010101010101010101010101" as Hex;
const BOB_KEY = "0x0202020202020202020202020202020202020202020202020202020202020202" as Hex;

interface NetworkConfig {
  label: string;
  rpcUrl: string;
  deployerPrivateKey: Hex;
  sp1Verifier: Hex | null;
  spawnAnvil: boolean;
}

function requireEnv(key: string): string {
  const v = process.env[key];
  if (!v) throw new Error(`${key} is not set; required for NETWORK=sepolia`);
  return v;
}

function buildNetworkConfig(): NetworkConfig {
  if (IS_SEPOLIA) {
    // The canonical SP1 gateway on Sepolia (SEPOLIA_SP1_VERIFIER) only routes
    // proofs whose verifier selector matches a verifier Succinct has
    // registered there. SP1 SDK 6.1.0's proof bytes don't currently route
    // through the gateway, so by default we deploy our own verifier (~3M
    // gas one-time). Set SP1_VERIFIER explicitly to override.
    const presetVerifier = process.env.SP1_VERIFIER as Hex | undefined;
    return {
      label: "sepolia",
      rpcUrl: requireEnv("SEPOLIA_RPC_URL"),
      deployerPrivateKey: (requireEnv("DEPLOYER_PRIVATE_KEY").startsWith("0x")
        ? requireEnv("DEPLOYER_PRIVATE_KEY")
        : `0x${requireEnv("DEPLOYER_PRIVATE_KEY")}`) as Hex,
      sp1Verifier: presetVerifier ?? null,
      spawnAnvil: false,
    };
  }
  return {
    label: "anvil",
    rpcUrl: `http://localhost:${ANVIL_PORT}`,
    // anvil[0] — fine for local because it isn't connected to anything outside.
    deployerPrivateKey: "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80" as Hex,
    sp1Verifier: null,
    spawnAnvil: true,
  };
}

// Network config is computed in main() so missing-env errors land in the
// shared catch handler with a clean message instead of a stack trace.
let NET: NetworkConfig;
let TOTAL_STEPS = 9;

const procs: Subprocess[] = [];
let stepCount = 0;

function log(...args: unknown[]) {
  console.log(...args);
}

function step(msg: string) {
  stepCount += 1;
  console.log(`\n[${stepCount}/${TOTAL_STEPS}] ${msg}`);
}

async function shutdown(code = 0) {
  log("\nshutting down child processes...");
  for (const p of procs) {
    try {
      p.kill();
    } catch {}
  }
  await Promise.all(procs.map((p) => p.exited.catch(() => {})));
  process.exit(code);
}

process.on("SIGINT", () => shutdown(130));
process.on("SIGTERM", () => shutdown(143));

async function waitFor(url: string, label: string, timeoutMs = 60_000) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    try {
      const r = await fetch(url, { signal: AbortSignal.timeout(1500) });
      if (r.ok) return;
    } catch {}
    await Bun.sleep(500);
  }
  throw new Error(`timed out waiting for ${label} at ${url}`);
}

async function spawnService(name: string, cmd: string[], env?: Record<string, string>) {
  const log = Bun.file(`/tmp/zk-rollup-${name}.log`);
  const p = spawn(cmd, {
    stdout: log,
    stderr: log,
    env: { ...process.env, ...env },
  });
  procs.push(p);
  return p;
}

function requireReleaseBinary(name: string) {
  const path = join(TARGET_RELEASE, name);
  if (!existsSync(path)) {
    throw new Error(
      `missing release binary: ${path}\nRun \`bun run build:rust\` first (or \`cargo build --release -p sp1-script -p prover-svc -p sequencer\`).`,
    );
  }
  return path;
}

function requireAnvilFree() {
  const r = Bun.spawnSync(["nc", "-z", "127.0.0.1", String(ANVIL_PORT)]);
  if (r.exitCode === 0) {
    throw new Error(
      `port ${ANVIL_PORT} is already in use; stop the existing process before running the demo`,
    );
  }
}

interface SignedTx {
  from: Hex;
  to: Hex;
  amount: bigint;
  nonce: bigint;
  signature: Hex;
}

function signingHash(from: Hex, to: Hex, amount: bigint, nonce: bigint): Hex {
  return keccak256(
    concat([
      hexToBytes(from),
      hexToBytes(to),
      numberToBytes(amount, { size: 8 }),
      numberToBytes(nonce, { size: 8 }),
    ]),
  );
}

async function buildSignedTx(
  account: ReturnType<typeof privateKeyToAccount>,
  to: Hex,
  amount: bigint,
  nonce: bigint,
): Promise<SignedTx> {
  const hash = signingHash(account.address, to, amount, nonce);
  // viem's `account.sign({ hash })` returns r(32) || s(32) || v(1) where v ∈ {27,28};
  // our Rust verifier accepts both that and {0,1}.
  const signature = await account.sign({ hash });
  return { from: account.address, to, amount, nonce, signature };
}

async function postJSON<T>(url: string, body: unknown, timeoutMs = 2 * 60 * 60 * 1000): Promise<T> {
  const r = await fetch(url, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
    signal: AbortSignal.timeout(timeoutMs),
  });
  if (!r.ok) {
    const text = await r.text();
    throw new Error(`${url} → ${r.status}: ${text}`);
  }
  return (await r.json()) as T;
}

async function getJSON<T>(url: string): Promise<T> {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`${url} → ${r.status}`);
  return (await r.json()) as T;
}

async function main() {
  const t0 = Date.now();
  NET = buildNetworkConfig();
  TOTAL_STEPS = NET.spawnAnvil ? 9 : 8;
  log(`SP1 zk-rollup demo (NETWORK=${NET.label}, PROOF_MODE=${PROOF_MODE})`);

  // ── 1. Preflight ─────────────────────────────────────────────────────
  step("Preflight: release binaries + ports");
  const proverBin = requireReleaseBinary("prover-svc");
  const sequencerBin = requireReleaseBinary("sequencer");
  const computeRootBin = requireReleaseBinary("compute-root");
  if (NET.spawnAnvil) requireAnvilFree();
  log(`  ✓ binaries present`);
  if (IS_SEPOLIA) {
    log(`  ✓ Sepolia mode: RPC=${NET.rpcUrl}`);
    log(`  ✓ SP1 verifier: ${NET.sp1Verifier ?? "(deploy fresh — mock mode on Sepolia)"}`);
  }

  // ── 2. anvil (skipped on Sepolia) ────────────────────────────────────
  if (NET.spawnAnvil) {
    step("Starting anvil on :8545");
    await spawnService("anvil", ["anvil", "--port", String(ANVIL_PORT), "--silent"]);
    const startedAt = Date.now();
    while (Date.now() - startedAt < 30_000) {
      try {
        const r = await fetch(NET.rpcUrl, {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ jsonrpc: "2.0", method: "web3_clientVersion", params: [], id: 1 }),
          signal: AbortSignal.timeout(1500),
        });
        if (r.ok) break;
      } catch {}
      await Bun.sleep(300);
    }
    log(`  ✓ anvil up`);
  }

  // ── 3. Genesis ───────────────────────────────────────────────────────
  step("Writing genesis.json");
  const alice = privateKeyToAccount(ALICE_KEY);
  const bob = privateKeyToAccount(BOB_KEY);
  const genesis = {
    accounts: [
      { address: alice.address, balance: 1_000_000, nonce: 0 },
      { address: bob.address, balance: 0, nonce: 0 },
    ],
  };
  await Bun.write(GENESIS_PATH, JSON.stringify(genesis, null, 2) + "\n");
  log(`  alice = ${alice.address}`);
  log(`  bob   = ${bob.address}`);

  // ── 4. compute-root ──────────────────────────────────────────────────
  step("Computing genesis root");
  const rootProc = Bun.spawnSync([computeRootBin, GENESIS_PATH]);
  if (rootProc.exitCode !== 0) {
    throw new Error(`compute-root failed: ${rootProc.stderr.toString()}`);
  }
  const genesisRoot = rootProc.stdout.toString().trim();
  log(`  GENESIS_ROOT = ${genesisRoot}`);

  // ── 5. prover-svc ────────────────────────────────────────────────────
  step(`Starting prover-svc on :${PROVER_PORT} (PROOF_MODE=${PROOF_MODE})`);
  // SP1_PROVER picks the *backend* (mock/cpu/network/cuda); PROOF_MODE picks
  // the proof *type* (mock vs groth16). The two must be compatible:
  //   - PROOF_MODE=mock pairs with SP1_PROVER=mock
  //   - PROOF_MODE=groth16 needs a real backend (cpu/network/cuda) — the
  //     mock backend returns degenerate proofs with empty bytes that fail
  //     on-chain verification.
  // We respect a user-provided SP1_PROVER, but veto mock when proof type
  // is groth16 — that combination is silently broken.
  let proverBackend = process.env.SP1_PROVER;
  if (PROOF_MODE !== "mock") {
    if (proverBackend === "mock") {
      log(`  ⚠ ignoring SP1_PROVER=mock from env: PROOF_MODE=${PROOF_MODE} requires a real backend; using cpu`);
      proverBackend = "cpu";
    }
    proverBackend = proverBackend ?? "cpu";
  } else {
    proverBackend = proverBackend ?? "mock";
  }
  await spawnService("prover-svc", [proverBin], {
    PROOF_MODE,
    SP1_PROVER: proverBackend,
    PROVER_SVC_PORT: String(PROVER_PORT),
  });
  if (PROOF_MODE !== "mock") {
    log(`  (groth16 first-run downloads the trusted setup; can take a few minutes)`);
  }
  await waitFor(`http://localhost:${PROVER_PORT}/health`, "prover-svc", 600_000);
  log(`  ✓ prover-svc up`);

  // ── 6. vkey ──────────────────────────────────────────────────────────
  step("Fetching program vkey");
  const proverInfo = await getJSON<{ mode: string; vkey: string }>(
    `http://localhost:${PROVER_PORT}/info`,
  );
  log(`  PROGRAM_VKEY = ${proverInfo.vkey}`);
  log(`  mode         = ${proverInfo.mode}`);

  // ── 7. Deploy Rollup ─────────────────────────────────────────────────
  step("Deploying Rollup");
  const forgeArgs = [
    "forge",
    "script",
    "script/Deploy.s.sol",
    "--rpc-url",
    NET.rpcUrl,
    "--broadcast",
    "--private-key",
    NET.deployerPrivateKey,
    "--json",
  ];
  if (IS_SEPOLIA) {
    // Sepolia confirmations are slow; let the broadcaster wait for inclusion.
    forgeArgs.push("--slow");
  }
  const deployEnv: Record<string, string> = {
    PROGRAM_VKEY: proverInfo.vkey,
    GENESIS_ROOT: genesisRoot,
    PROOF_MODE,
  };
  if (NET.sp1Verifier) deployEnv.SP1_VERIFIER = NET.sp1Verifier;
  const forge = Bun.spawnSync(forgeArgs, {
    cwd: CONTRACTS,
    env: { ...process.env, ...deployEnv },
  });
  if (forge.exitCode !== 0) {
    throw new Error(
      `forge script failed:\n${forge.stdout.toString()}\n${forge.stderr.toString()}`,
    );
  }
  const stdout = forge.stdout.toString().trim();
  let rollupAddress: Hex | null = null;
  for (const line of stdout.split("\n")) {
    try {
      const obj = JSON.parse(line);
      if (obj.returns?.rollup?.value) {
        rollupAddress = obj.returns.rollup.value as Hex;
        break;
      }
    } catch {}
  }
  if (!rollupAddress) {
    throw new Error(`could not parse Rollup address from forge output:\n${stdout}`);
  }
  log(`  Rollup at    = ${rollupAddress}`);

  // ── 8. sequencer ─────────────────────────────────────────────────────
  step(`Starting sequencer on :${SEQUENCER_PORT}`);
  await spawnService("sequencer", [sequencerBin], {
    DEPLOYER_PRIVATE_KEY: NET.deployerPrivateKey,
    ROLLUP_ADDRESS: rollupAddress,
    L1_RPC_URL: NET.rpcUrl,
    PROVER_SVC_URL: `http://localhost:${PROVER_PORT}`,
    SEQUENCER_PORT: String(SEQUENCER_PORT),
    GENESIS_PATH,
  });
  await waitFor(`http://localhost:${SEQUENCER_PORT}/health`, "sequencer", 30_000);
  log(`  ✓ sequencer up`);

  // ── 9. Drive the rollup ──────────────────────────────────────────────
  step("Submitting 2 transfers and triggering batch");
  const transfers = [
    { amount: 100n, nonce: 0n },
    { amount: 200n, nonce: 1n },
  ];
  for (const { amount, nonce } of transfers) {
    const tx = await buildSignedTx(alice, bob.address, amount, nonce);
    const resp = await postJSON<{
      accepted: boolean;
      mempool_size: number;
      speculative_root: string;
    }>(`http://localhost:${SEQUENCER_PORT}/tx`, {
      from: tx.from,
      to: tx.to,
      amount: Number(tx.amount),
      nonce: Number(tx.nonce),
      signature: tx.signature,
    });
    log(
      `  tx alice→bob ${tx.amount}, nonce=${tx.nonce} → mempool=${resp.mempool_size}, speculative=${resp.speculative_root.slice(0, 18)}…`,
    );
  }

  if (PROOF_MODE !== "mock") {
    log(`  triggering batch — groth16 proving locally can take many minutes...`);
  }
  const proveStarted = Date.now();
  // Bun's fetch silently hits an internal idle timeout if the response body
  // doesn't stream for several minutes (the Groth16 prover blocks the socket
  // while it churns). Shell out to curl with a 2-hour limit instead.
  const curlResult = Bun.spawnSync([
    "curl",
    "-sS",
    "-m",
    String(2 * 60 * 60),
    "-X",
    "POST",
    "-H",
    "content-type: application/json",
    "-d",
    "{}",
    `http://localhost:${SEQUENCER_PORT}/batch`,
  ]);
  if (curlResult.exitCode !== 0) {
    throw new Error(
      `curl /batch failed (exit ${curlResult.exitCode}):\n${curlResult.stderr.toString()}`,
    );
  }
  const batchRaw = curlResult.stdout.toString();
  const batchParsed = JSON.parse(batchRaw);
  if (batchParsed.error) {
    throw new Error(`/batch returned error: ${batchParsed.error}`);
  }
  const batch = batchParsed as {
    batch_number: number;
    txs: number;
    prev_root: string;
    new_root: string;
    batch_hash: string;
    l1_tx_hash: string;
    l1_block: number;
    gas_used: number;
  };

  const proveSecs = ((Date.now() - proveStarted) / 1000).toFixed(1);
  log(`\n═══ Batch #${batch.batch_number} settled (prove + L1 in ${proveSecs}s) ═══`);
  log(`  prev_root  = ${batch.prev_root}`);
  log(`  new_root   = ${batch.new_root}`);
  log(`  batch_hash = ${batch.batch_hash}`);
  log(`  l1_tx_hash = ${batch.l1_tx_hash}`);
  if (IS_SEPOLIA) {
    log(`               https://sepolia.etherscan.io/tx/${batch.l1_tx_hash}`);
  }
  log(`  l1_block   = ${batch.l1_block}`);
  log(`  gas_used   = ${batch.gas_used.toLocaleString()}`);

  log(`\n--- Final state (from sequencer) ---`);
  for (const [name, account] of [
    ["alice", alice] as const,
    ["bob", bob] as const,
  ]) {
    const acc = await getJSON<{ address: string; balance: number; nonce: number }>(
      `http://localhost:${SEQUENCER_PORT}/state/${account.address.toLowerCase()}`,
    );
    log(`  ${name.padEnd(5)} balance=${acc.balance.toString().padStart(8)} nonce=${acc.nonce}`);
  }

  log(`\nDemo finished in ${((Date.now() - t0) / 1000).toFixed(1)}s.`);
  log(`Logs: /tmp/zk-rollup-anvil.log /tmp/zk-rollup-prover-svc.log /tmp/zk-rollup-sequencer.log`);
  log(`Press Ctrl-C to stop background services.`);

  await new Promise(() => {});
}

main().catch((e) => {
  console.error("\nERROR:", e instanceof Error ? e.message : e);
  shutdown(1);
});
