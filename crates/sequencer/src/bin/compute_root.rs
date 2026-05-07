//! Tiny CLI: read a genesis JSON file and print its merkle root as a
//! 0x-prefixed bytes32 hex string. Used by the deploy script flow so
//! `GENESIS_ROOT` matches what the sequencer would compute at startup.

use anyhow::{Context, Result};
use serde::Deserialize;
use shared_types::{Account, Address, State};
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct GenesisAccount {
    address: String,
    balance: u64,
    #[serde(default)]
    nonce: u64,
}

#[derive(Debug, Deserialize)]
struct Genesis {
    accounts: Vec<GenesisAccount>,
}

fn main() -> Result<()> {
    let path: PathBuf = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: compute_root <genesis.json>"))?
        .into();
    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let genesis: Genesis = serde_json::from_slice(&bytes)?;

    let mut state = State::new();
    for a in genesis.accounts {
        let addr = Address::from_hex(&a.address).map_err(|e| anyhow::anyhow!("bad address {}: {e:?}", a.address))?;
        state.set(
            addr,
            Account {
                balance: a.balance,
                nonce: a.nonce,
            },
        );
    }
    let root = state.merkle_root();
    println!("0x{}", hex::encode(root));
    Ok(())
}
