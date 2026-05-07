//! Pure state transition function for the rollup. Imported by both the SP1 program
//! (where it runs in the zkVM) and the sequencer (which speculatively applies the same
//! logic to its in-memory state to keep mempool answers consistent with what will be proved).

extern crate alloc;

use shared_types::{recover_address, Batch, Hash, PublicValuesStruct, RecoverError, State, Tx};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum StfError {
    BadSignature,
    NonceMismatch { expected: u64, got: u64 },
    InsufficientBalance { have: u64, need: u64 },
    Overflow,
    SelfTransfer,
}

impl From<RecoverError> for StfError {
    fn from(_: RecoverError) -> Self {
        StfError::BadSignature
    }
}

pub fn apply_tx(state: &mut State, tx: &Tx) -> Result<(), StfError> {
    if tx.from == tx.to {
        return Err(StfError::SelfTransfer);
    }

    let recovered = recover_address(&tx.signing_hash(), &tx.signature)?;
    if recovered != tx.from {
        return Err(StfError::BadSignature);
    }

    let mut from_acc = state.get(&tx.from);
    if from_acc.nonce != tx.nonce {
        return Err(StfError::NonceMismatch {
            expected: from_acc.nonce,
            got: tx.nonce,
        });
    }
    if from_acc.balance < tx.amount {
        return Err(StfError::InsufficientBalance {
            have: from_acc.balance,
            need: tx.amount,
        });
    }

    let mut to_acc = state.get(&tx.to);
    let new_to_balance = to_acc.balance.checked_add(tx.amount).ok_or(StfError::Overflow)?;

    from_acc.balance -= tx.amount;
    from_acc.nonce += 1;
    to_acc.balance = new_to_balance;

    state.set(tx.from, from_acc);
    state.set(tx.to, to_acc);

    Ok(())
}

pub struct ApplyOutput {
    pub new_state: State,
    pub public_values: PublicValuesStruct,
}

/// Applies all txs in `batch` against `prev_state`. Returns the new state plus the public values
/// that the SP1 program will commit (and the L1 contract will verify against its stored root).
pub fn apply_batch(prev_state: State, batch: &Batch) -> Result<ApplyOutput, StfError> {
    let prev_root: Hash = prev_state.merkle_root();
    let mut state = prev_state;

    for tx in &batch.txs {
        apply_tx(&mut state, tx)?;
    }

    let new_root = state.merkle_root();
    let batch_hash = batch.hash();

    let public_values = PublicValuesStruct {
        prevRoot: prev_root.into(),
        newRoot: new_root.into(),
        batchHash: batch_hash.into(),
    };

    Ok(ApplyOutput {
        new_state: state,
        public_values,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::{signature::hazmat::PrehashSigner, RecoveryId, Signature, SigningKey};
    use shared_types::{Account, Address};
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

        fn sign(&self, hash: &Hash) -> alloc::vec::Vec<u8> {
            let (sig, rec): (Signature, RecoveryId) = self.sk.sign_prehash(hash).unwrap();
            let mut out = alloc::vec![0u8; 65];
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
            signature: alloc::vec![0u8; 65],
        };
        tx.signature = from.sign(&tx.signing_hash());
        tx
    }

    fn genesis_with(alice_balance: u64, bob_balance: u64, alice: Address, bob: Address) -> State {
        let mut s = State::new();
        s.set(alice, Account { balance: alice_balance, nonce: 0 });
        s.set(bob, Account { balance: bob_balance, nonce: 0 });
        s
    }

    #[test]
    fn happy_path_transfer() {
        let alice = Wallet::new(1);
        let bob = Wallet::new(2);
        let state = genesis_with(1000, 0, alice.addr, bob.addr);

        let tx = signed_tx(&alice, bob.addr, 250, 0);
        let batch = Batch { txs: alloc::vec![tx] };

        let out = apply_batch(state, &batch).unwrap();
        assert_eq!(out.new_state.get(&alice.addr).balance, 750);
        assert_eq!(out.new_state.get(&alice.addr).nonce, 1);
        assert_eq!(out.new_state.get(&bob.addr).balance, 250);
    }

    #[test]
    fn rejects_bad_nonce() {
        let alice = Wallet::new(1);
        let bob = Wallet::new(2);
        let state = genesis_with(1000, 0, alice.addr, bob.addr);

        let tx = signed_tx(&alice, bob.addr, 100, 5);
        let batch = Batch { txs: alloc::vec![tx] };
        let err = match apply_batch(state, &batch) {
            Err(e) => e,
            Ok(_) => panic!("expected error"),
        };
        assert!(matches!(err, StfError::NonceMismatch { expected: 0, got: 5 }));
    }

    #[test]
    fn rejects_insufficient_balance() {
        let alice = Wallet::new(1);
        let bob = Wallet::new(2);
        let state = genesis_with(50, 0, alice.addr, bob.addr);

        let tx = signed_tx(&alice, bob.addr, 100, 0);
        let batch = Batch { txs: alloc::vec![tx] };
        let err = match apply_batch(state, &batch) {
            Err(e) => e,
            Ok(_) => panic!("expected error"),
        };
        assert!(matches!(err, StfError::InsufficientBalance { have: 50, need: 100 }));
    }

    #[test]
    fn rejects_invalid_signature() {
        let alice = Wallet::new(1);
        let bob = Wallet::new(2);
        let state = genesis_with(1000, 0, alice.addr, bob.addr);

        let mut tx = signed_tx(&alice, bob.addr, 100, 0);
        tx.signature[5] ^= 0xFF;
        let batch = Batch { txs: alloc::vec![tx] };
        let err = match apply_batch(state, &batch) {
            Err(e) => e,
            Ok(_) => panic!("expected error"),
        };
        assert!(matches!(err, StfError::BadSignature));
    }

    #[test]
    fn sequential_batch_advances_root_deterministically() {
        let alice = Wallet::new(1);
        let bob = Wallet::new(2);
        let s0 = genesis_with(1000, 0, alice.addr, bob.addr);
        let r0 = s0.merkle_root();

        let txs = alloc::vec![
            signed_tx(&alice, bob.addr, 100, 0),
            signed_tx(&alice, bob.addr, 200, 1),
        ];
        let batch = Batch { txs };

        let out_a = apply_batch(s0.clone(), &batch).unwrap();
        let out_b = apply_batch(s0, &batch).unwrap();
        assert_eq!(out_a.new_state.merkle_root(), out_b.new_state.merkle_root());
        assert_ne!(out_a.new_state.merkle_root(), r0);
        assert_eq!(out_a.new_state.get(&alice.addr).balance, 700);
        assert_eq!(out_a.new_state.get(&bob.addr).balance, 300);
    }
}
