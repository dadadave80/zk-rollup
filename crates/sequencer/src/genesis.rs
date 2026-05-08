use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use shared_types::{Account, Address, State};
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct GenesisAccount {
    pub address: String,
    pub balance: u64,
    #[serde(default)]
    pub nonce: u64,
}

#[derive(Debug, Deserialize)]
pub struct Genesis {
    pub accounts: Vec<GenesisAccount>,
}

impl Genesis {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("read genesis file: {}", path.display()))?;
        let parsed: Genesis = serde_json::from_slice(&bytes).context("parse genesis JSON")?;
        Ok(parsed)
    }

    pub fn into_state(self) -> Result<State> {
        let mut state = State::new();
        for entry in self.accounts {
            let addr = Address::from_hex(&entry.address)
                .map_err(|e| anyhow!("bad genesis address {}: {e:?}", entry.address))?;
            state.set(
                addr,
                Account {
                    balance: entry.balance,
                    nonce: entry.nonce,
                },
            );
        }
        Ok(state)
    }
}
