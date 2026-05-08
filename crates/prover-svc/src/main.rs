//! HTTP wrapper around `sp1-script`. Exposes the SP1 zkVM program as a service
//! the sequencer (and the Bun orchestrator, indirectly through the sequencer)
//! can call without depending on the SP1 SDK directly.
//!
//! Endpoints:
//!
//!   GET  /health                              → "ok"
//!   GET  /info                                → { mode, vkey }
//!   GET  /vkey                                → { vkey }
//!   POST /execute  { prev_state, batch }      → { public_values, prev_root, new_root, batch_hash, cycles }
//!   POST /prove    { prev_state, batch }      → { proof, public_values, vkey, prev_root, new_root, batch_hash }
//!
//! Proof mode is selected by the `PROOF_MODE` env var (mock or groth16); the
//! same binary serves both — only the prove path differs.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    extract::State as AxumState,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use shared_types::{Batch, PublicValuesStruct, State};
use sp1_script::{ProofMode, ProveOutput};
use thiserror::Error;
use tracing::{error, info};

#[derive(Clone)]
struct AppState {
    mode: ProofMode,
    vkey: Arc<String>,
}

#[derive(Debug, Deserialize)]
struct WorkRequest {
    prev_state: State,
    batch: Batch,
}

#[derive(Debug, Serialize)]
struct InfoResponse {
    mode: &'static str,
    vkey: String,
}

#[derive(Debug, Serialize)]
struct VkeyResponse {
    vkey: String,
}

#[derive(Debug, Serialize)]
struct ExecuteResponse {
    public_values: String,
    prev_root: String,
    new_root: String,
    batch_hash: String,
    cycles: u64,
}

#[derive(Debug, Serialize)]
struct ProveResponse {
    proof: String,
    public_values: String,
    vkey: String,
    prev_root: String,
    new_root: String,
    batch_hash: String,
}

#[derive(Debug, Error)]
enum ApiError {
    #[error("internal error: {0}")]
    Internal(String),
    #[error("bad request: {0}")]
    BadRequest(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, msg) = match &self {
            ApiError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m.clone()),
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m.clone()),
        };
        error!(error = %self, "request failed");
        (status, Json(serde_json::json!({ "error": msg }))).into_response()
    }
}

async fn health() -> &'static str {
    "ok"
}

async fn info(AxumState(state): AxumState<AppState>) -> Json<InfoResponse> {
    Json(InfoResponse {
        mode: match state.mode {
            ProofMode::Mock => "mock",
            ProofMode::Groth16 => "groth16",
        },
        vkey: (*state.vkey).clone(),
    })
}

async fn vkey(AxumState(state): AxumState<AppState>) -> Json<VkeyResponse> {
    Json(VkeyResponse {
        vkey: (*state.vkey).clone(),
    })
}

fn pv_hex(pv: &PublicValuesStruct, bytes: &[u8]) -> ExecuteResponse {
    ExecuteResponse {
        public_values: hex0x(bytes),
        prev_root: hex0x(<[u8; 32]>::from(pv.prevRoot).as_slice()),
        new_root: hex0x(<[u8; 32]>::from(pv.newRoot).as_slice()),
        batch_hash: hex0x(<[u8; 32]>::from(pv.batchHash).as_slice()),
        cycles: 0,
    }
}

fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

/// SP1's blocking SDK calls `block_on` internally, which panics from inside any
/// Tokio runtime — including a `spawn_blocking` thread, since that pool is part of
/// the runtime. Running the work on a real OS thread sidesteps the runtime check.
async fn run_off_runtime<T, F>(f: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.await
        .map_err(|e| ApiError::Internal(format!("worker thread dropped: {e}")))?
        .map_err(|e| ApiError::BadRequest(e.to_string()))
}

async fn execute(Json(req): Json<WorkRequest>) -> Result<Json<ExecuteResponse>, ApiError> {
    info!(txs = req.batch.txs.len(), "execute");
    let out = run_off_runtime(move || sp1_script::execute_only(&req.prev_state, &req.batch)).await?;
    let mut resp = pv_hex(&out.public_values, &out.public_values_bytes);
    resp.cycles = out.cycles;
    Ok(Json(resp))
}

async fn prove(
    AxumState(state): AxumState<AppState>,
    Json(req): Json<WorkRequest>,
) -> Result<Json<ProveResponse>, ApiError> {
    let mode = state.mode;
    info!(txs = req.batch.txs.len(), ?mode, "prove");
    let out: ProveOutput =
        run_off_runtime(move || sp1_script::prove(&req.prev_state, &req.batch, mode)).await?;
    Ok(Json(ProveResponse {
        proof: hex0x(&out.proof_bytes),
        public_values: hex0x(&out.public_values_bytes),
        vkey: out.vkey_bytes32.clone(),
        prev_root: hex0x(<[u8; 32]>::from(out.public_values.prevRoot).as_slice()),
        new_root: hex0x(<[u8; 32]>::from(out.public_values.newRoot).as_slice()),
        batch_hash: hex0x(<[u8; 32]>::from(out.public_values.batchHash).as_slice()),
    }))
}

fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    // SP1's setup_logger installs a tracing dispatcher; don't double-install.
    sp1_sdk::utils::setup_logger();

    let mode = ProofMode::from_env();

    // vkey_bytes32 uses SP1's blocking SDK, which spins up its own Tokio
    // runtime via block_on — that conflicts with #[tokio::main]. Compute it
    // before any Tokio context exists.
    let program_vkey = sp1_script::vkey_bytes32()?;
    info!(?mode, vkey = %program_vkey, "computed program vkey");

    let state = AppState {
        mode,
        vkey: Arc::new(program_vkey),
    };

    let port: u16 = std::env::var("PROVER_SVC_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(7002);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
        let app = Router::new()
            .route("/health", get(health))
            .route("/info", get(info))
            .route("/vkey", get(vkey))
            .route("/execute", post(execute))
            .route("/prove", post(prove))
            .with_state(state);

        let listener = tokio::net::TcpListener::bind(addr).await?;
        info!("prover-svc listening on http://{addr}");
        axum::serve(listener, app).await?;
        Ok::<_, anyhow::Error>(())
    })
}

