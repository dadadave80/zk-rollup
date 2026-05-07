//! Drives a running sequencer end-to-end: signs transfers, POSTs them to /tx,
//! triggers /batch, and prints the result. Used as a CLI replacement until the
//! Bun orchestrator wraps the same flow.

use anyhow::{Context, Result};
use k256::ecdsa::{signature::hazmat::PrehashSigner, RecoveryId, Signature, SigningKey};
use serde::{Deserialize, Serialize};
use shared_types::{Address, Hash, Tx};
use tiny_keccak::{Hasher, Keccak};

#[derive(Serialize)]
struct TxRequest<'a> {
    from: &'a str,
    to: &'a str,
    amount: u64,
    nonce: u64,
    signature: &'a str,
}

#[derive(Debug, Deserialize)]
struct TxResponse {
    accepted: bool,
    mempool_size: usize,
    speculative_root: String,
}

#[derive(Debug, Deserialize)]
struct BatchResponse {
    batch_number: u64,
    txs: usize,
    prev_root: String,
    new_root: String,
    batch_hash: String,
    l1_tx_hash: String,
    l1_block: u64,
    gas_used: u64,
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

fn sign(sk: &SigningKey, hash: &Hash) -> String {
    let (sig, rec): (Signature, RecoveryId) = sk.sign_prehash(hash).unwrap();
    let mut out = vec![0u8; 65];
    out[..64].copy_from_slice(&sig.to_bytes());
    out[64] = rec.to_byte();
    format!("0x{}", hex::encode(out))
}

fn main() -> Result<()> {
    let base = std::env::var("SEQUENCER_URL").unwrap_or_else(|_| "http://localhost:7001".to_string());
    let client = reqwest::blocking::Client::new();

    let (alice_sk, alice) = make_wallet(1);
    let (_, bob) = make_wallet(2);
    println!("alice = {}", alice.to_hex());
    println!("bob   = {}", bob.to_hex());

    // Build two signed transfers: alice → bob, 100 then 200.
    let mut to_submit = Vec::new();
    for (amount, nonce) in [(100u64, 0u64), (200u64, 1u64)] {
        let mut tx = Tx { from: alice, to: bob, amount, nonce, signature: vec![0u8; 65] };
        let h = tx.signing_hash();
        tx.signature = hex::decode(sign(&alice_sk, &h).strip_prefix("0x").unwrap()).unwrap();
        to_submit.push(tx);
    }

    for tx in &to_submit {
        let req = TxRequest {
            from: &tx.from.to_hex(),
            to: &tx.to.to_hex(),
            amount: tx.amount,
            nonce: tx.nonce,
            signature: &format!("0x{}", hex::encode(&tx.signature)),
        };
        let resp: TxResponse = client
            .post(format!("{base}/tx"))
            .json(&req)
            .send()?
            .error_for_status()?
            .json()?;
        println!("submitted tx nonce={} → mempool_size={} speculative_root={}", tx.nonce, resp.mempool_size, resp.speculative_root);
        assert!(resp.accepted);
    }

    println!("\n--- POST /batch ---");
    let resp = client.post(format!("{base}/batch")).send()?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        return Err(anyhow::anyhow!("/batch failed ({status}): {body}"));
    }
    let batch: BatchResponse = resp.json().context("decode /batch response")?;
    println!("batch_number = {}", batch.batch_number);
    println!("txs          = {}", batch.txs);
    println!("prev_root    = {}", batch.prev_root);
    println!("new_root     = {}", batch.new_root);
    println!("batch_hash   = {}", batch.batch_hash);
    println!("l1_tx_hash   = {}", batch.l1_tx_hash);
    println!("l1_block     = {}", batch.l1_block);
    println!("gas_used     = {}", batch.gas_used);

    println!("\n--- GET /root after settle ---");
    let root: serde_json::Value = client
        .get(format!("{base}/root"))
        .send()?
        .error_for_status()?
        .json()?;
    println!("{}", serde_json::to_string_pretty(&root).unwrap());

    Ok(())
}
