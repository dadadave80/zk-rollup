//! Smoke-test binary: builds a hardcoded 2-tx batch, runs the SP1 program in execute-only mode,
//! and prints prevRoot/newRoot/batchHash. No proving. Used at the end of H0–5 to verify the
//! entire program path works before we wire up the real prover and L1 contracts.

use anyhow::Result;
use k256::ecdsa::{signature::hazmat::PrehashSigner, RecoveryId, Signature, SigningKey};
use shared_types::{Account, Address, Batch, Hash, State, Tx};
use sp1_script::execute_only;
use tiny_keccak::{Hasher, Keccak};

struct Wallet {
    sk: SigningKey,
    addr: Address,
}

impl Wallet {
    fn new(seed: u8) -> Self {
        let sk = SigningKey::from_bytes(&[seed; 32].into()).unwrap();
        let vk = sk.verifying_key();
        let pub_bytes = vk.to_encoded_point(false);
        let mut k = Keccak::v256();
        k.update(&pub_bytes.as_bytes()[1..]);
        let mut h = [0u8; 32];
        k.finalize(&mut h);
        let mut addr = [0u8; 20];
        addr.copy_from_slice(&h[12..]);
        Self { sk, addr: Address(addr) }
    }

    fn sign(&self, hash: &Hash) -> Vec<u8> {
        let (sig, rec): (Signature, RecoveryId) = self.sk.sign_prehash(hash).unwrap();
        let mut out = vec![0u8; 65];
        out[..64].copy_from_slice(&sig.to_bytes());
        out[64] = rec.to_byte();
        out
    }
}

fn signed_tx(from: &Wallet, to: Address, amount: u64, nonce: u64) -> Tx {
    let mut tx = Tx {
        from: from.addr,
        to,
        amount,
        nonce,
        signature: vec![0u8; 65],
    };
    tx.signature = from.sign(&tx.signing_hash());
    tx
}

fn main() -> Result<()> {
    sp1_sdk::utils::setup_logger();

    let alice = Wallet::new(1);
    let bob = Wallet::new(2);

    let mut state = State::new();
    state.set(alice.addr, Account { balance: 1000, nonce: 0 });
    state.set(bob.addr, Account { balance: 0, nonce: 0 });
    let prev_root = state.merkle_root();

    let batch = Batch {
        txs: vec![
            signed_tx(&alice, bob.addr, 100, 0),
            signed_tx(&alice, bob.addr, 200, 1),
        ],
    };

    println!("alice = {:?}", alice.addr);
    println!("bob   = {:?}", bob.addr);
    println!("prev  = 0x{}", hex::encode(prev_root));
    println!("running SP1 program in execute-only mode...");

    let out = execute_only(&state, &batch)?;

    println!("\n--- public values ---");
    println!("prevRoot  = 0x{}", hex::encode(<[u8; 32]>::from(out.public_values.prevRoot)));
    println!("newRoot   = 0x{}", hex::encode(<[u8; 32]>::from(out.public_values.newRoot)));
    println!("batchHash = 0x{}", hex::encode(<[u8; 32]>::from(out.public_values.batchHash)));
    println!("cycles    = {}", out.cycles);

    // Sanity: prevRoot the program committed must equal the host's prev_root.
    let committed_prev: [u8; 32] = out.public_values.prevRoot.into();
    assert_eq!(committed_prev, prev_root, "host/zkVM prev_root mismatch");
    println!("\nOK: zkVM and host agree on prev_root");

    Ok(())
}
