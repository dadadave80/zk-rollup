//! Smoke test: builds a valid signed batch in-process and POSTs it to a running
//! prover-svc, exercising /execute and /prove. Reads PROVER_SVC_URL from env
//! (default http://localhost:7002).

use anyhow::{anyhow, Result};
use k256::ecdsa::{signature::hazmat::PrehashSigner, RecoveryId, Signature, SigningKey};
use serde::{Deserialize, Serialize};
use shared_types::{Account, Address, Batch, Hash, State, Tx};
use tiny_keccak::{Hasher, Keccak};

#[derive(Serialize)]
struct WorkRequest<'a> {
    prev_state: &'a State,
    batch: &'a Batch,
}

#[derive(Debug, Deserialize)]
struct ExecuteResponse {
    public_values: String,
    prev_root: String,
    new_root: String,
    batch_hash: String,
    cycles: u64,
}

#[derive(Debug, Deserialize)]
struct ProveResponse {
    proof: String,
    public_values: String,
    vkey: String,
    prev_root: String,
    new_root: String,
    batch_hash: String,
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
    let base = std::env::var("PROVER_SVC_URL").unwrap_or_else(|_| "http://localhost:7002".to_string());
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2 * 60 * 60))
        .build()?;

    let (alice_sk, alice) = make_wallet(1);
    let (_, bob) = make_wallet(2);

    let mut state = State::new();
    state.set(alice, Account { balance: 1000, nonce: 0 });
    state.set(bob, Account { balance: 0, nonce: 0 });
    let host_prev_root = state.merkle_root();

    let batch = Batch {
        txs: vec![
            signed_tx(&alice_sk, alice, bob, 100, 0),
            signed_tx(&alice_sk, alice, bob, 200, 1),
        ],
    };
    let host_batch_hash = batch.hash();

    let payload = WorkRequest { prev_state: &state, batch: &batch };

    println!("--- POST /execute ---");
    let exec: ExecuteResponse = client
        .post(format!("{base}/execute"))
        .json(&payload)
        .send()?
        .error_for_status()?
        .json()?;
    println!("prev_root  = {}", exec.prev_root);
    println!("new_root   = {}", exec.new_root);
    println!("batch_hash = {}", exec.batch_hash);
    println!("cycles     = {}", exec.cycles);

    let prev_root_hex = format!("0x{}", hex::encode(host_prev_root));
    let batch_hash_hex = format!("0x{}", hex::encode(host_batch_hash));
    if exec.prev_root != prev_root_hex {
        return Err(anyhow!("prev_root mismatch: got {}, expected {}", exec.prev_root, prev_root_hex));
    }
    if exec.batch_hash != batch_hash_hex {
        return Err(anyhow!("batch_hash mismatch: got {}, expected {}", exec.batch_hash, batch_hash_hex));
    }
    println!("OK: /execute prev_root and batch_hash match host computation");

    println!("\n--- POST /prove ---");
    let prove: ProveResponse = client
        .post(format!("{base}/prove"))
        .json(&payload)
        .send()?
        .error_for_status()?
        .json()?;
    println!("vkey       = {}", prove.vkey);
    println!("public_vals= {} ({} bytes)", &prove.public_values[..18], (prove.public_values.len() - 2) / 2);
    println!("proof      = {} ({} bytes)", if prove.proof.len() > 18 { &prove.proof[..18] } else { &prove.proof }, (prove.proof.len() - 2) / 2);
    if prove.prev_root != prev_root_hex {
        return Err(anyhow!("/prove prev_root mismatch"));
    }
    if prove.batch_hash != batch_hash_hex {
        return Err(anyhow!("/prove batch_hash mismatch"));
    }
    if prove.new_root != exec.new_root {
        return Err(anyhow!("execute and prove disagree on new_root: {} vs {}", exec.new_root, prove.new_root));
    }
    println!("OK: /prove agrees with /execute on roots");

    Ok(())
}
