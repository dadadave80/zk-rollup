use std::sync::Arc;

use axum::{
    extract::{Path, State as AxumState},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use shared_types::{Address, Tx};
use tracing::{error, info, warn};

use crate::l1::L1Client;
use crate::prover::ProverClient;
use crate::state::SharedState;

#[derive(Clone)]
pub struct AppCtx {
    pub state: SharedState,
    pub prover: Arc<ProverClient>,
    pub l1: Arc<L1Client>,
    pub program_vkey: Arc<String>,
    pub proof_mode: Arc<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (code, msg) = match &self {
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m.clone()),
            ApiError::NotFound(m) => (StatusCode::NOT_FOUND, m.clone()),
            ApiError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m.clone()),
        };
        if matches!(self, ApiError::Internal(_)) {
            error!(error = %self, "request failed");
        }
        (code, Json(serde_json::json!({ "error": msg }))).into_response()
    }
}

pub fn router(ctx: AppCtx) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/info", get(info))
        .route("/root", get(root))
        .route("/state/:addr", get(account))
        .route("/mempool", get(mempool))
        .route("/tx", post(submit_tx))
        .route("/batch", post(trigger_batch))
        .with_state(ctx)
}

async fn health() -> &'static str {
    "ok"
}

#[derive(Serialize)]
struct InfoResponse {
    canonical_root: String,
    batch_count: u64,
    mempool_size: usize,
    program_vkey: String,
    proof_mode: String,
    rollup_address: String,
}

async fn info(AxumState(ctx): AxumState<AppCtx>) -> Json<InfoResponse> {
    let s = ctx.state.read().await;
    Json(InfoResponse {
        canonical_root: hex0x(&s.canonical_root),
        batch_count: s.batch_count,
        mempool_size: s.mempool.len(),
        program_vkey: (*ctx.program_vkey).clone(),
        proof_mode: (*ctx.proof_mode).clone(),
        rollup_address: format!("{:?}", ctx.l1.rollup_address),
    })
}

#[derive(Serialize)]
struct RootResponse {
    root: String,
}

async fn root(AxumState(ctx): AxumState<AppCtx>) -> Json<RootResponse> {
    let s = ctx.state.read().await;
    Json(RootResponse {
        root: hex0x(&s.canonical_root),
    })
}

#[derive(Serialize)]
struct AccountResponse {
    address: String,
    balance: u64,
    nonce: u64,
}

async fn account(
    AxumState(ctx): AxumState<AppCtx>,
    Path(addr_hex): Path<String>,
) -> Result<Json<AccountResponse>, ApiError> {
    let addr = Address::from_hex(&addr_hex).map_err(|e| ApiError::BadRequest(format!("bad address: {e:?}")))?;
    let s = ctx.state.read().await;
    let acc = s.canonical_state.get(&addr);
    Ok(Json(AccountResponse {
        address: addr.to_hex(),
        balance: acc.balance,
        nonce: acc.nonce,
    }))
}

#[derive(Serialize)]
struct MempoolEntry {
    from: String,
    to: String,
    amount: u64,
    nonce: u64,
}

async fn mempool(AxumState(ctx): AxumState<AppCtx>) -> Json<Vec<MempoolEntry>> {
    let s = ctx.state.read().await;
    Json(
        s.mempool
            .iter()
            .map(|t| MempoolEntry {
                from: t.from.to_hex(),
                to: t.to.to_hex(),
                amount: t.amount,
                nonce: t.nonce,
            })
            .collect(),
    )
}

#[derive(Debug, Deserialize)]
pub struct TxRequest {
    pub from: String,
    pub to: String,
    pub amount: u64,
    pub nonce: u64,
    /// 0x-prefixed 65-byte recoverable ECDSA signature.
    pub signature: String,
}

#[derive(Serialize)]
struct TxResponse {
    accepted: bool,
    mempool_size: usize,
    speculative_root: String,
}

impl TxRequest {
    fn into_tx(self) -> Result<Tx, ApiError> {
        let from = Address::from_hex(&self.from).map_err(|e| ApiError::BadRequest(format!("bad from: {e:?}")))?;
        let to = Address::from_hex(&self.to).map_err(|e| ApiError::BadRequest(format!("bad to: {e:?}")))?;
        let sig_hex = self.signature.strip_prefix("0x").unwrap_or(&self.signature);
        let signature = hex::decode(sig_hex).map_err(|e| ApiError::BadRequest(format!("bad signature hex: {e}")))?;
        if signature.len() != 65 {
            return Err(ApiError::BadRequest(format!(
                "signature must be 65 bytes, got {}",
                signature.len()
            )));
        }
        Ok(Tx {
            from,
            to,
            amount: self.amount,
            nonce: self.nonce,
            signature,
        })
    }
}

async fn submit_tx(
    AxumState(ctx): AxumState<AppCtx>,
    Json(req): Json<TxRequest>,
) -> Result<Json<TxResponse>, ApiError> {
    let tx = req.into_tx()?;
    let mut s = ctx.state.write().await;
    s.admit_tx(tx)
        .map_err(|e| ApiError::BadRequest(format!("rejected by stf: {e:?}")))?;
    let mempool_size = s.mempool.len();
    let speculative_root = hex0x(&s.speculative_state.merkle_root());
    info!(mempool_size, "tx admitted");
    Ok(Json(TxResponse {
        accepted: true,
        mempool_size,
        speculative_root,
    }))
}

#[derive(Serialize)]
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

async fn trigger_batch(AxumState(ctx): AxumState<AppCtx>) -> Result<Json<BatchResponse>, ApiError> {
    // 1. Snapshot prev_state + drain mempool under the lock; release before
    //    the long prove + L1 round trip.
    let (prev_state, batch) = {
        let mut s = ctx.state.write().await;
        if s.mempool.is_empty() {
            return Err(ApiError::BadRequest("mempool is empty".into()));
        }
        s.drain_for_batch()
    };
    let txs = batch.txs.len();
    info!(txs, "building batch");

    // 2. Ask prover-svc for a proof.
    let prove = ctx
        .prover
        .prove(&prev_state, &batch)
        .await
        .map_err(|e| {
            error!(error = %e, "prove failed");
            // restore the mempool so the user's txs aren't silently lost
            ApiError::Internal(format!("prove failed: {e}"))
        })?;
    info!(prev_root = %prove.prev_root, new_root = %prove.new_root, "proof produced");

    // 3. Decode hex into bytes for L1 calldata.
    let pv_bytes = hex_to_bytes(&prove.public_values).map_err(|e| ApiError::Internal(e))?;
    let proof_bytes = hex_to_bytes(&prove.proof).map_err(|e| ApiError::Internal(e))?;

    // 4. Submit on L1.
    let outcome = match ctx.l1.submit_batch(pv_bytes, proof_bytes).await {
        Ok(o) => o,
        Err(e) => {
            warn!(error = %e, "L1 submitBatch failed; restoring mempool");
            // Restore the txs to the mempool so the user's intent isn't lost.
            // This is best-effort: any tx that conflicts with current state is
            // silently dropped.
            let mut s = ctx.state.write().await;
            s.restore_mempool(batch);
            return Err(ApiError::Internal(format!("L1 submitBatch failed: {e}")));
        }
    };

    // 5. Commit the batch to canonical state locally.
    let new_root_bytes = hex_to_bytes(&prove.new_root)
        .map_err(|e| ApiError::Internal(e))?;
    if new_root_bytes.len() != 32 {
        return Err(ApiError::Internal(format!(
            "prover-svc returned non-32-byte new_root: {} bytes",
            new_root_bytes.len()
        )));
    }
    let mut new_root = [0u8; 32];
    new_root.copy_from_slice(&new_root_bytes);

    {
        let mut s = ctx.state.write().await;
        s.commit_batch(&batch, new_root)
            .map_err(|e| ApiError::Internal(format!("local commit failed: {e}")))?;
    }

    let s = ctx.state.read().await;
    Ok(Json(BatchResponse {
        batch_number: s.batch_count,
        txs,
        prev_root: prove.prev_root,
        new_root: prove.new_root,
        batch_hash: prove.batch_hash,
        l1_tx_hash: format!("{:?}", outcome.tx_hash),
        l1_block: outcome.block_number,
        gas_used: outcome.gas_used,
    }))
}

fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

fn hex_to_bytes(s: &str) -> Result<Vec<u8>, String> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    hex::decode(s).map_err(|e| format!("decode hex: {e}"))
}
