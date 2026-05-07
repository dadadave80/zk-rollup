//! Host-side glue around the SP1 SDK: builds stdin from (State, Batch), runs either an
//! execution-only pass (fast, for sequencer/sanity checks) or full proof generation
//! (mock or Groth16, for L1 settlement).

use alloy_sol_types::SolType;
use anyhow::{anyhow, Context, Result};
use shared_types::{Batch, PublicValuesStruct, State};
use sp1_sdk::{
    blocking::{ProveRequest, Prover, ProverClient},
    include_elf, Elf, HashableKey, ProvingKey, SP1ProofWithPublicValues, SP1Stdin,
};

pub const STF_ELF: Elf = include_elf!("sp1-program-stf");

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum ProofMode {
    /// SP1 mock prover — instant, paired with `SP1MockVerifier` on-chain.
    #[default]
    Mock,
    /// Real Groth16 proof — paired with `SP1VerifierGroth16` (or the verifier gateway) on-chain.
    Groth16,
}

impl ProofMode {
    pub fn from_env() -> Self {
        match std::env::var("PROOF_MODE")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "groth16" | "real" => ProofMode::Groth16,
            _ => ProofMode::Mock,
        }
    }
}

pub struct ExecuteOutput {
    pub public_values: PublicValuesStruct,
    pub public_values_bytes: Vec<u8>,
    pub cycles: u64,
}

pub struct ProveOutput {
    pub proof_bytes: Vec<u8>,
    pub public_values: PublicValuesStruct,
    pub public_values_bytes: Vec<u8>,
    pub vkey_bytes32: String,
}

fn build_stdin(prev_state: &State, batch: &Batch) -> SP1Stdin {
    let mut stdin = SP1Stdin::new();
    stdin.write(prev_state);
    stdin.write(batch);
    stdin
}

/// Execute the program inside SP1's RISC-V emulator without proving — fast (~100ms for small batches).
/// Use this from the sequencer to sanity-check that a batch will succeed before invoking the prover.
pub fn execute_only(prev_state: &State, batch: &Batch) -> Result<ExecuteOutput> {
    let client = ProverClient::from_env();
    let stdin = build_stdin(prev_state, batch);
    let (output, report) = client
        .execute(STF_ELF, stdin)
        .run()
        .map_err(|e| anyhow!("sp1 execute failed: {e}"))?;
    let bytes = output.as_slice().to_vec();
    let public_values = PublicValuesStruct::abi_decode(&bytes)
        .map_err(|e| anyhow!("failed to decode public values: {e}"))?;
    Ok(ExecuteOutput {
        public_values,
        public_values_bytes: bytes,
        cycles: report.total_instruction_count(),
    })
}

/// Generate a real proof. Returns proof bytes encoded the way the on-chain SP1 verifier expects them.
pub fn prove(prev_state: &State, batch: &Batch, mode: ProofMode) -> Result<ProveOutput> {
    let client = ProverClient::from_env();
    let pk = client.setup(STF_ELF).context("failed to setup proving key")?;
    let vkey_bytes32 = pk.verifying_key().bytes32().to_string();
    let stdin = build_stdin(prev_state, batch);

    let proof: SP1ProofWithPublicValues = match mode {
        ProofMode::Mock => client
            .prove(&pk, stdin)
            .run()
            .map_err(|e| anyhow!("sp1 mock prove failed: {e}"))?,
        ProofMode::Groth16 => client
            .prove(&pk, stdin)
            .groth16()
            .run()
            .map_err(|e| anyhow!("sp1 groth16 prove failed: {e}"))?,
    };

    let public_values_bytes = proof.public_values.as_slice().to_vec();
    let public_values = PublicValuesStruct::abi_decode(&public_values_bytes)
        .map_err(|e| anyhow!("failed to decode committed public values: {e}"))?;

    Ok(ProveOutput {
        proof_bytes: proof.bytes(),
        public_values,
        public_values_bytes,
        vkey_bytes32,
    })
}

/// Compute the program verification key as the bytes32 hex string the on-chain Rollup expects.
pub fn vkey_bytes32() -> Result<String> {
    use sp1_sdk::blocking::MockProver;
    let prover = MockProver::new();
    let pk = prover
        .setup(STF_ELF)
        .map_err(|e| anyhow!("failed to setup proving key: {e}"))?;
    Ok(pk.verifying_key().bytes32().to_string())
}
