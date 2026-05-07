# contracts

Foundry project for the on-chain side of the zk-rollup. See `../README.md` for
the wider architecture.

## Layout

- `src/Rollup.sol` — settlement contract. Holds the canonical L2 state root,
  the immutable program vkey, and the verifier address. `submitBatch` calls
  `ISP1Verifier.verifyProof`, abi-decodes `(prevRoot, newRoot, batchHash)`,
  rejects stale prev roots and zero new roots, then advances state.
- `test/Rollup.t.sol` — 9 tests against `SP1MockVerifier`: constructor
  guards, happy path, stale prev, zero new root, mock-empty-proof check,
  multi-batch chaining.
- `script/Deploy.s.sol` — env-driven deploy used by both anvil and Sepolia.

## Submodules

`lib/forge-std` and `lib/sp1-contracts` are git submodules — the SP1 verifier
suite (`SP1MockVerifier`, `SP1VerifierGateway`, Groth16/Plonk verifiers
v1.0.1 → v6.1.0) is vendored here so we don't depend on a Succinct deployment
existing on every chain.

## Running tests

```shell
forge build
forge test
```

## Deploying

The deploy script is driven entirely by env so the same script works on local
anvil and on Sepolia:

| Env | Required? | Description |
|---|---|---|
| `PROGRAM_VKEY` | yes | bytes32 verification key. Get it from `cargo run -p sp1-script --bin vkey` or `curl http://prover-svc/vkey`. |
| `GENESIS_ROOT` | yes | Initial L2 state root. The sequencer's `compute-root` CLI prints this from a genesis JSON. |
| `PROOF_MODE` | no (default `mock`) | `mock` deploys `SP1MockVerifier`; `groth16` requires an explicit verifier address. |
| `SP1_VERIFIER` | required for `groth16` | Address of the SP1 verifier on the target chain. Sepolia gateway: `0x3B6041173B80E77f038f3F2C0f9744f04837185e`. |

Local anvil:

```shell
PROGRAM_VKEY=0x… GENESIS_ROOT=0x… PROOF_MODE=mock \
forge script script/Deploy.s.sol \
  --rpc-url http://localhost:8545 \
  --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 \
  --broadcast
```

Sepolia:

```shell
PROGRAM_VKEY=0x… GENESIS_ROOT=0x… \
PROOF_MODE=groth16 \
SP1_VERIFIER=0x3B6041173B80E77f038f3F2C0f9744f04837185e \
forge script script/Deploy.s.sol \
  --rpc-url $SEPOLIA_RPC_URL \
  --private-key $DEPLOYER_PRIVATE_KEY \
  --broadcast
```

## Public values encoding

The SP1 program commits the public values via `alloy_sol_types::sol!`-defined
struct, so `Rollup.submitBatch` can decode them as a plain ABI tuple:

```solidity
(bytes32 prevRoot, bytes32 newRoot, bytes32 batchHash) =
    abi.decode(publicValues, (bytes32, bytes32, bytes32));
```

The same struct is defined once in `crates/shared-types` and re-used in the
zkVM program — there's no separate Solidity-side schema to drift.
