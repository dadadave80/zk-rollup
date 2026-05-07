//! Run the SP1 program against a hardcoded batch and produce a proof in the chosen mode.
//! Mostly a sanity tool; the real prover is invoked over HTTP from `prover-svc` once that
//! crate is wired up.

use anyhow::Result;
use clap::{Parser, ValueEnum};
use k256::ecdsa::{signature::hazmat::PrehashSigner, RecoveryId, Signature, SigningKey};
use shared_types::{Account, Address, Batch, Hash, State, Tx};
use sp1_script::{prove, ProofMode};
use tiny_keccak::{Hasher, Keccak};

#[derive(Copy, Clone, Debug, ValueEnum)]
enum Mode {
    Mock,
    Groth16,
}

impl From<Mode> for ProofMode {
    fn from(m: Mode) -> Self {
        match m {
            Mode::Mock => ProofMode::Mock,
            Mode::Groth16 => ProofMode::Groth16,
        }
    }
}

#[derive(Parser)]
struct Args {
    #[arg(long, value_enum, default_value = "mock")]
    mode: Mode,
}

fn make_wallet(seed: u8) -> (SigningKey, Address) {
    let sk = SigningKey::from_bytes(&[seed; 32].into()).unwrap();
    let vk = sk.verifying_key();
    let pub_bytes = vk.to_encoded_point(false);
    let mut k = Keccak::v256();
    k.update(&pub_bytes.as_bytes()[1..]);
    let mut h = [0u8; 32];
    k.finalize(&mut h);
    let mut addr = [0u8; 20];
    addr.copy_from_slice(&h[12..]);
    (sk, Address(addr))
}

fn signed_tx(sk: &SigningKey, from: Address, to: Address, amount: u64, nonce: u64) -> Tx {
    let mut tx = Tx { from, to, amount, nonce, signature: vec![0u8; 65] };
    let h: Hash = tx.signing_hash();
    let (sig, rec): (Signature, RecoveryId) = sk.sign_prehash(&h).unwrap();
    let mut out = vec![0u8; 65];
    out[..64].copy_from_slice(&sig.to_bytes());
    out[64] = rec.to_byte();
    tx.signature = out;
    tx
}

fn main() -> Result<()> {
    sp1_sdk::utils::setup_logger();
    let args = Args::parse();

    let (alice_sk, alice) = make_wallet(1);
    let (_, bob) = make_wallet(2);

    let mut state = State::new();
    state.set(alice, Account { balance: 1000, nonce: 0 });
    state.set(bob, Account { balance: 0, nonce: 0 });

    let batch = Batch {
        txs: vec![signed_tx(&alice_sk, alice, bob, 100, 0)],
    };

    println!("running SP1 prover in {:?} mode...", args.mode);
    let out = prove(&state, &batch, args.mode.into())?;
    println!("vkey       = {}", out.vkey_bytes32);
    println!("publicVals = 0x{}", hex::encode(&out.public_values_bytes));
    println!("proof      = 0x{} ({} bytes)", hex::encode(&out.proof_bytes), out.proof_bytes.len());
    Ok(())
}
