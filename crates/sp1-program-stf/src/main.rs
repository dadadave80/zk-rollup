//! SP1 zkVM program: applies a batch of signed transfers to the prior account state and commits
//! `(prevRoot, newRoot, batchHash)` as ABI-encoded public values for the on-chain Rollup contract.

#![no_main]
sp1_zkvm::entrypoint!(main);

use alloy_sol_types::SolType;
use shared_types::{Batch, PublicValuesStruct, State};

pub fn main() {
    // The host writes a single bincode-serialized (State, Batch) tuple via SP1Stdin.
    let prev_state: State = sp1_zkvm::io::read::<State>();
    let batch: Batch = sp1_zkvm::io::read::<Batch>();

    let out = match stf::apply_batch(prev_state, &batch) {
        Ok(o) => o,
        Err(e) => panic!("stf rejected batch: {:?}", e),
    };

    let bytes = PublicValuesStruct::abi_encode(&out.public_values);
    sp1_zkvm::io::commit_slice(&bytes);
}
