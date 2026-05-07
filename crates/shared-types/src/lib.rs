//! Canonical types shared between the SP1 zkVM program, sequencer, prover-svc, and host tooling.
//!
//! The on-chain Rollup contract decodes public values as `(bytes32, bytes32, bytes32)` ABI tuple,
//! so `PublicValuesStruct` is defined via `alloy_sol_types::sol!` to keep the Rust ↔ Solidity
//! boundary tight.

extern crate alloc;

use alloy_sol_types::sol;
use serde::{Deserialize, Serialize};
use tiny_keccak::{Hasher, Keccak};

pub type Hash = [u8; 32];

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct Address(pub [u8; 20]);

impl Address {
    pub const ZERO: Address = Address([0u8; 20]);

    pub fn to_hex(&self) -> alloc::string::String {
        let mut out = alloc::string::String::with_capacity(42);
        out.push_str("0x");
        out.push_str(&hex::encode(self.0));
        out
    }

    pub fn from_hex(s: &str) -> Result<Address, hex::FromHexError> {
        let s = s.strip_prefix("0x").unwrap_or(s);
        let bytes = hex::decode(s)?;
        if bytes.len() != 20 {
            return Err(hex::FromHexError::InvalidStringLength);
        }
        let mut a = [0u8; 20];
        a.copy_from_slice(&bytes);
        Ok(Address(a))
    }
}

impl core::fmt::Debug for Address {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Tx {
    pub from: Address,
    pub to: Address,
    pub amount: u64,
    pub nonce: u64,
    /// Recoverable ECDSA signature over `signing_hash(self)`: r(32) || s(32) || v(1).
    /// Always 65 bytes; stored as Vec<u8> only because serde's array derives stop at 32.
    #[serde(with = "serde_bytes")]
    pub signature: alloc::vec::Vec<u8>,
}

impl Tx {
    /// Hash the canonical signing payload: keccak256(from || to || amount_be || nonce_be).
    /// The signature is over this hash; recovering the signer must yield `from`.
    pub fn signing_hash(&self) -> Hash {
        let mut k = Keccak::v256();
        k.update(&self.from.0);
        k.update(&self.to.0);
        k.update(&self.amount.to_be_bytes());
        k.update(&self.nonce.to_be_bytes());
        let mut out = [0u8; 32];
        k.finalize(&mut out);
        out
    }

    /// Hash the full transaction including the signature — used to compute the batch_hash
    /// public value so the L1 contract can audit which txs were included.
    pub fn full_hash(&self) -> Hash {
        let mut k = Keccak::v256();
        k.update(&self.from.0);
        k.update(&self.to.0);
        k.update(&self.amount.to_be_bytes());
        k.update(&self.nonce.to_be_bytes());
        k.update(&self.signature);
        let mut out = [0u8; 32];
        k.finalize(&mut out);
        out
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub balance: u64,
    pub nonce: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// Sorted by address for deterministic root computation.
    pub accounts: alloc::vec::Vec<(Address, Account)>,
}

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, addr: &Address) -> Account {
        self.accounts
            .binary_search_by_key(addr, |(a, _)| *a)
            .map(|idx| self.accounts[idx].1)
            .unwrap_or_default()
    }

    pub fn set(&mut self, addr: Address, account: Account) {
        match self.accounts.binary_search_by_key(&addr, |(a, _)| *a) {
            Ok(idx) => self.accounts[idx].1 = account,
            Err(idx) => self.accounts.insert(idx, (addr, account)),
        }
    }

    /// Compute root: keccak256( for each (addr, balance, nonce) in sorted order: addr || balance_be || nonce_be ).
    pub fn merkle_root(&self) -> Hash {
        let mut k = Keccak::v256();
        for (addr, acc) in &self.accounts {
            k.update(&addr.0);
            k.update(&acc.balance.to_be_bytes());
            k.update(&acc.nonce.to_be_bytes());
        }
        let mut out = [0u8; 32];
        k.finalize(&mut out);
        out
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Batch {
    pub txs: alloc::vec::Vec<Tx>,
}

impl Batch {
    /// keccak256 over each tx's full_hash, sequentially.
    pub fn hash(&self) -> Hash {
        let mut k = Keccak::v256();
        for tx in &self.txs {
            k.update(&tx.full_hash());
        }
        let mut out = [0u8; 32];
        k.finalize(&mut out);
        out
    }
}

/// What the host passes to the zkVM via stdin (bincode-encoded).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StfInput {
    pub prev_state: State,
    pub batch: Batch,
}

sol! {
    /// Public values committed by the SP1 program. `Rollup.sol` decodes these via `abi.decode`.
    struct PublicValuesStruct {
        bytes32 prevRoot;
        bytes32 newRoot;
        bytes32 batchHash;
    }
}

/// Recover an Ethereum-style address from a recoverable secp256k1 signature over `msg_hash`.
/// Signature layout: r(32) || s(32) || v(1) where v ∈ {0, 1, 27, 28}.
pub fn recover_address(msg_hash: &Hash, signature: &[u8]) -> Result<Address, RecoverError> {
    use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};

    if signature.len() != 65 {
        return Err(RecoverError::BadSignature);
    }
    let sig = Signature::from_slice(&signature[..64]).map_err(|_| RecoverError::BadSignature)?;
    let v = signature[64];
    let v_norm = match v {
        0 | 1 => v,
        27 | 28 => v - 27,
        _ => return Err(RecoverError::BadRecoveryId),
    };
    let rec = RecoveryId::try_from(v_norm).map_err(|_| RecoverError::BadRecoveryId)?;
    let vk = VerifyingKey::recover_from_prehash(msg_hash, &sig, rec)
        .map_err(|_| RecoverError::Recover)?;
    let pub_bytes = vk.to_encoded_point(false);
    let pub_bytes = pub_bytes.as_bytes();
    // Skip the 0x04 prefix byte: hash the 64-byte uncompressed pubkey, take last 20.
    let mut k = Keccak::v256();
    k.update(&pub_bytes[1..]);
    let mut hashed = [0u8; 32];
    k.finalize(&mut hashed);
    let mut addr = [0u8; 20];
    addr.copy_from_slice(&hashed[12..]);
    Ok(Address(addr))
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum RecoverError {
    BadSignature,
    BadRecoveryId,
    Recover,
}

#[cfg(feature = "std")]
impl core::fmt::Display for RecoverError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            RecoverError::BadSignature => write!(f, "malformed signature"),
            RecoverError::BadRecoveryId => write!(f, "invalid recovery id"),
            RecoverError::Recover => write!(f, "key recovery failed"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for RecoverError {}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use k256::ecdsa::{signature::hazmat::PrehashSigner, RecoveryId, Signature, SigningKey};

    fn signer_address(sk: &SigningKey) -> Address {
        let vk = sk.verifying_key();
        let pub_bytes = vk.to_encoded_point(false);
        let mut k = Keccak::v256();
        k.update(&pub_bytes.as_bytes()[1..]);
        let mut h = [0u8; 32];
        k.finalize(&mut h);
        let mut addr = [0u8; 20];
        addr.copy_from_slice(&h[12..]);
        Address(addr)
    }

    fn sign_tx(sk: &SigningKey, hash: &Hash) -> alloc::vec::Vec<u8> {
        let (sig, rec): (Signature, RecoveryId) = sk.sign_prehash(hash).unwrap();
        let mut out = vec![0u8; 65];
        out[..64].copy_from_slice(&sig.to_bytes());
        out[64] = rec.to_byte();
        out
    }

    #[test]
    fn round_trip_recover() {
        let sk = SigningKey::from_bytes(&[7u8; 32].into()).unwrap();
        let from = signer_address(&sk);
        let tx = Tx {
            from,
            to: Address([1u8; 20]),
            amount: 100,
            nonce: 0,
            signature: vec![0u8; 65],
        };
        let h = tx.signing_hash();
        let sig = sign_tx(&sk, &h);
        let recovered = recover_address(&h, &sig).unwrap();
        assert_eq!(recovered, from);
    }

    #[test]
    fn state_root_is_deterministic() {
        let mut a = State::new();
        a.set(Address([1u8; 20]), Account { balance: 100, nonce: 1 });
        a.set(Address([2u8; 20]), Account { balance: 50, nonce: 0 });

        let mut b = State::new();
        b.set(Address([2u8; 20]), Account { balance: 50, nonce: 0 });
        b.set(Address([1u8; 20]), Account { balance: 100, nonce: 1 });

        assert_eq!(a.merkle_root(), b.merkle_root());
    }

    #[test]
    fn empty_batch_hash_is_empty_keccak() {
        let b = Batch { txs: vec![] };
        // Empty keccak256.
        assert_eq!(
            hex::encode(b.hash()),
            "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
        );
    }
}
