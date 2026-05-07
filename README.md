# zk-rollup

A working SP1-based zk-rollup. A simple-ledger L2 with signed transfers, settled
on an L1 contract that verifies a Groth16 proof produced by the SP1 zkVM.

```text
┌──────────┐  POST /tx      ┌─────────────┐  POST /prove   ┌─────────────┐
│  Client  │──────────────▶│  sequencer   │──────────────▶│ prover-svc   │
│  (bun /  │               │ (axum, Rust) │                │ (axum, Rust) │
│   curl)  │               │ mempool +    │                │  sp1-script  │
└──────────┘               │ speculative  │                │     ┃        │
                           │ state        │                │     ▼        │
                           └─────┬────────┘                │ ┌──────────┐ │
                                 │ submitBatch             │ │  zkVM    │ │
                                 ▼ (alloy)                 │ │ program  │ │
                           ┌──────────────┐                │ │ stf::    │ │
                           │  Rollup.sol  │                │ │ apply_   │ │
                           │  on anvil /  │                │ │ batch    │ │
                           │  Sepolia     │                │ └──────────┘ │
                           │  ISP1Verifier│                └──────────────┘
                           └──────────────┘
```

The SP1 program commits `(prevRoot, newRoot, batchHash)` as ABI-encoded public
values. The on-chain `Rollup.submitBatch` calls `ISP1Verifier.verifyProof`,
checks `prev == storedRoot`, and advances state.

## Quick start

```bash
# 1. Install JS deps and SP1 toolchain
bun install
curl -L https://sp1.succinct.xyz | bash && sp1up

# 2. Build the Rust binaries (sp1-script, prover-svc, sequencer, demo, compute-root)
bun run build:rust

# 3. Build the Solidity contracts
bun run build:contracts

# 4. End-to-end demo on a local anvil with mock proofs (~4 seconds)
bun run demo
```

`bun run demo` owns the full lifecycle — it spawns anvil, prover-svc and
sequencer, deploys `Rollup.sol`, signs two transfers from a seeded wallet,
triggers a batch, prints L1 settlement details and final balances, and parks
so you can poke the running services. Press Ctrl-C to tear everything down.

For real Groth16 proofs end-to-end (slower, ~minutes per proof):

```bash
bun run demo:groth16
```

## Repo layout

```
zk-rollup/
├── index.ts                    # Bun orchestrator (the headline demo path)
├── package.json                # bun scripts: demo, demo:groth16, build:rust, build:contracts
├── Cargo.toml                  # Rust workspace
│
├── crates/
│   ├── shared-types/           # Address, Tx, Account, State, Batch, PublicValuesStruct
│   ├── stf/                    # apply_tx + apply_batch (pure, no_std-friendly)
│   ├── sp1-program-stf/        # zkVM binary; commits (prevRoot, newRoot, batchHash)
│   ├── sp1-script/             # host: execute_only, prove(mode), vkey_bytes32
│   ├── prover-svc/             # axum HTTP service wrapping sp1-script
│   ├── sequencer/              # axum HTTP service: mempool, speculative state, L1 client
│   ├── batcher / circuits / circuit-verifier / node /
│   └── sp1-program-agg / sp1-program-circ / state          # stubs reserved for v2
│
└── contracts/                  # Foundry project
    ├── src/Rollup.sol          # SP1 verifier integration + state root
    ├── test/Rollup.t.sol       # 9 tests against SP1MockVerifier
    ├── script/Deploy.s.sol     # env-driven deploy (mock or groth16)
    └── lib/sp1-contracts       # vendored SP1 verifiers (submodule)
```

## Architecture

**State transition function (`crates/stf`).** Pure Rust. Each tx is an
ECDSA-signed `(from, to, amount, nonce)` transfer; the recovered signer must
equal `tx.from`. State is a sorted `Vec<(Address, Account)>` whose merkle root
is `keccak(addr ‖ balance_be ‖ nonce_be)` for each account in order.

**zkVM program (`crates/sp1-program-stf`).** Reads `(State, Batch)` from SP1
stdin, calls `stf::apply_batch`, commits the abi-encoded `PublicValuesStruct`
defined via `alloy_sol_types::sol!`. The Solidity contract decodes the same
tuple with `abi.decode((bytes32, bytes32, bytes32))`.

**prover-svc (`crates/prover-svc`).** Wraps `sp1-script` behind HTTP. Each
SP1 call runs on a dedicated `std::thread` because the SP1 SDK's blocking API
calls `block_on` internally and panics if it sees an outer Tokio runtime.

**Sequencer (`crates/sequencer`).** Mempool + two layered states:
`canonical_state` mirrors the L1 root, `speculative_state` is canonical with
all unbatched txs applied. New `/tx` requests are validated against the
speculative copy so back-to-back transfers from the same sender chain
correctly. `/batch` drains the mempool, calls `prover-svc /prove`, submits
`Rollup.submitBatch` via alloy, then re-applies locally and asserts the
local merkle root matches what the proof committed.

**Rollup contract (`contracts/src/Rollup.sol`).** Holds `bytes32 stateRoot`,
the immutable `programVKey`, and the `verifier` address. `submitBatch` calls
`ISP1Verifier.verifyProof` (which reverts on a bad proof), decodes the public
values, rejects stale prev root and zero new root, then advances state.

## HTTP API

Sequencer (default `:7001`):

```text
GET  /health                          → "ok"
GET  /info                            → { canonical_root, batch_count, mempool_size, … }
GET  /root                            → { root }
GET  /state/:addr                     → { address, balance, nonce }
GET  /mempool                         → [{ from, to, amount, nonce }, …]
POST /tx { from, to, amount, nonce, signature }   → { accepted, mempool_size, speculative_root }
POST /batch                           → { batch_number, prev_root, new_root, batch_hash, l1_tx_hash, l1_block, gas_used }
```

Prover-svc (default `:7002`):

```text
GET  /health
GET  /info                            → { mode, vkey }
GET  /vkey
POST /execute  { prev_state, batch }  → { public_values, prev_root, new_root, batch_hash, cycles }
POST /prove    { prev_state, batch }  → { proof, public_values, vkey, prev_root, new_root, batch_hash }
```

## Manual run-through

If you want to drive each piece by hand instead of using `bun run demo`:

```bash
# Terminal 1 — L1
anvil --port 8545

# Terminal 2 — prover-svc (computes the program vkey at startup)
PROOF_MODE=mock SP1_PROVER=mock cargo run --release -p prover-svc

# Read the vkey
curl -s http://localhost:7002/vkey

# Compute the genesis root for your genesis.json
cargo run --release -p sequencer --bin compute-root -- ./genesis.json

# Deploy
cd contracts && \
  PROGRAM_VKEY=0x… GENESIS_ROOT=0x… PROOF_MODE=mock \
  forge script script/Deploy.s.sol \
    --rpc-url http://localhost:8545 \
    --private-key 0xac0974… \
    --broadcast

# Terminal 3 — sequencer
DEPLOYER_PRIVATE_KEY=0xac0974… \
ROLLUP_ADDRESS=0x… \
L1_RPC_URL=http://localhost:8545 \
PROVER_SVC_URL=http://localhost:7002 \
GENESIS_PATH=./genesis.json \
cargo run --release -p sequencer

# Submit and settle
cargo run --release -p sequencer --bin demo
```

## Configuration

See `.env.example` for the full list. The orchestrator hardcodes anvil[0]'s
private key for local runs; for Sepolia override `DEPLOYER_PRIVATE_KEY`,
`L1_RPC_URL`, `L1_CHAIN_ID`, and pass an explicit `SP1_VERIFIER` (the
canonical Sepolia gateway is `0x3B6041173B80E77f038f3F2C0f9744f04837185e`).

## Testing

```bash
cargo test                             # stf (5) + shared-types (3)
cd contracts && forge test             # Rollup (9) against SP1MockVerifier
cargo run --release -p prover-svc --bin smoke   # POSTs a signed batch to a running prover-svc
```

## What's intentionally out of scope

These are deliberately left as `cargo new --lib` stubs — they're scaffolding
for v2 work and aren't on the demo path:

- `sp1-program-agg`, `sp1-program-circ` — multi-batch proof aggregation
- `circuits`, `circuit-verifier` — redundant with the on-chain SP1 verifier
- `batcher` — folded into the sequencer
- `node` — passive sync; the sequencer is the source of truth for the demo

## Cloning

This repo uses git submodules for `forge-std` and `sp1-contracts`. Clone
recursively:

```bash
git clone --recursive https://github.com/dadadave80/zk-rollup
# or, if you cloned without --recursive:
git submodule update --init --recursive
```
