use alloy::network::EthereumWallet;
use alloy::primitives::{Address as EthAddress, Bytes, B256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use alloy::sol;
use anyhow::{anyhow, Context, Result};

sol! {
    #[sol(rpc)]
    contract Rollup {
        function stateRoot() external view returns (bytes32);
        function batchCount() external view returns (uint64);
        function programVKey() external view returns (bytes32);
        function verifier() external view returns (address);
        function submitBatch(bytes calldata publicValues, bytes calldata proofBytes) external;
        event BatchSettled(uint64 indexed batchNumber, bytes32 prevRoot, bytes32 newRoot, bytes32 batchHash);
    }
}

/// Lightweight L1 client. We don't keep a long-lived provider in a field —
/// the alloy filler generic is unwieldy and each call rebuilds the provider
/// (cheap; just an HTTP transport wrapper). This keeps the type simple.
#[derive(Clone)]
pub struct L1Client {
    pub rollup_address: EthAddress,
    rpc_url: String,
    wallet: EthereumWallet,
}

pub struct SettleOutcome {
    pub tx_hash: B256,
    pub gas_used: u64,
    pub block_number: u64,
}

impl L1Client {
    pub fn connect(rpc_url: &str, private_key: &str, rollup_address: &str) -> Result<Self> {
        let signer: PrivateKeySigner = private_key
            .strip_prefix("0x")
            .unwrap_or(private_key)
            .parse()
            .context("parse DEPLOYER_PRIVATE_KEY")?;
        let wallet = EthereumWallet::from(signer);
        let rollup_address: EthAddress = rollup_address.parse().context("parse ROLLUP_ADDRESS")?;
        Ok(Self {
            rollup_address,
            rpc_url: rpc_url.to_string(),
            wallet,
        })
    }

    fn provider(&self) -> Result<impl Provider + Clone> {
        let url = self.rpc_url.parse().context("parse L1_RPC_URL")?;
        Ok(ProviderBuilder::new().wallet(self.wallet.clone()).connect_http(url))
    }

    pub async fn state_root(&self) -> Result<[u8; 32]> {
        let r = Rollup::new(self.rollup_address, self.provider()?);
        let root = r.stateRoot().call().await?;
        Ok(root.0)
    }

    pub async fn batch_count(&self) -> Result<u64> {
        let r = Rollup::new(self.rollup_address, self.provider()?);
        Ok(r.batchCount().call().await?)
    }

    pub async fn submit_batch(&self, public_values: Vec<u8>, proof_bytes: Vec<u8>) -> Result<SettleOutcome> {
        let rollup = Rollup::new(self.rollup_address, self.provider()?);
        let pending = rollup
            .submitBatch(Bytes::from(public_values), Bytes::from(proof_bytes))
            .send()
            .await
            .context("send submitBatch")?;
        let tx_hash = *pending.tx_hash();
        let receipt = pending.get_receipt().await.context("await submitBatch receipt")?;
        if !receipt.status() {
            return Err(anyhow!(
                "submitBatch reverted in tx {tx_hash} (block {:?})",
                receipt.block_number
            ));
        }
        Ok(SettleOutcome {
            tx_hash,
            gas_used: receipt.gas_used as u64,
            block_number: receipt.block_number.unwrap_or_default(),
        })
    }
}
