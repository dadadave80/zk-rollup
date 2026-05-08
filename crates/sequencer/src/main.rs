//! HTTP sequencer for the SP1 rollup. Owns the in-memory mempool and
//! speculative state, delegates proof generation to prover-svc over HTTP,
//! and settles batches on L1 via alloy. See routes.rs for endpoints.

mod genesis;
mod l1;
mod prover;
mod routes;
mod state;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use tokio::sync::RwLock;
use tracing::info;

use crate::genesis::Genesis;
use crate::l1::L1Client;
use crate::prover::ProverClient;
use crate::routes::{router, AppCtx};
use crate::state::SequencerState;

#[derive(Parser)]
struct Args {
    /// Path to the genesis JSON file (overrides GENESIS_PATH).
    #[arg(long)]
    genesis: Option<PathBuf>,
}

fn env_or<T>(key: &str, default: T) -> T
where
    T: std::str::FromStr,
{
    std::env::var(key).ok().and_then(|s| s.parse().ok()).unwrap_or(default)
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    let genesis_path: PathBuf = args
        .genesis
        .or_else(|| std::env::var("GENESIS_PATH").ok().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("./genesis.json"));
    let genesis = Genesis::load(&genesis_path).context("load genesis")?;
    let initial_state = genesis.into_state()?;
    let initial_root = initial_state.merkle_root();
    info!(genesis = %genesis_path.display(), root = %hex::encode(initial_root), "loaded genesis");

    let prover_url = std::env::var("PROVER_SVC_URL").unwrap_or_else(|_| "http://localhost:7002".to_string());
    let prover = ProverClient::new(prover_url.clone());
    let prover_info = prover.info().await.context("query prover-svc /info")?;
    info!(prover = %prover_url, mode = %prover_info.mode, vkey = %prover_info.vkey, "connected to prover-svc");

    let l1_rpc = std::env::var("L1_RPC_URL").unwrap_or_else(|_| "http://localhost:8545".to_string());
    let l1_pk = std::env::var("DEPLOYER_PRIVATE_KEY").context("DEPLOYER_PRIVATE_KEY not set")?;
    let rollup_address = std::env::var("ROLLUP_ADDRESS").context("ROLLUP_ADDRESS not set")?;
    let l1 = L1Client::connect(&l1_rpc, &l1_pk, &rollup_address).context("connect L1")?;
    let on_chain_root = l1.state_root().await.context("read L1 stateRoot")?;
    if on_chain_root != initial_root {
        anyhow::bail!(
            "genesis mismatch: local computed root {} but L1 stateRoot is {} \
             — re-deploy Rollup with this genesis or update genesis.json",
            hex::encode(initial_root),
            hex::encode(on_chain_root)
        );
    }
    info!(rollup = %rollup_address, root = %hex::encode(on_chain_root), "L1 root matches genesis");

    let state = Arc::new(RwLock::new(SequencerState::from_genesis(initial_state)));
    let ctx = AppCtx {
        state,
        prover: Arc::new(prover),
        l1: Arc::new(l1),
        program_vkey: Arc::new(prover_info.vkey),
        proof_mode: Arc::new(prover_info.mode),
    };

    let app = router(ctx);
    let port: u16 = env_or("SEQUENCER_PORT", 7001u16);
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("sequencer listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
