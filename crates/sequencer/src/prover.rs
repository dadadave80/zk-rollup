use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use shared_types::{Batch, State};

/// Thin async client for prover-svc. The sequencer calls /prove for batch
/// settlement and /info at startup to discover the program vkey + mode.
#[derive(Clone)]
pub struct ProverClient {
    base: String,
    http: reqwest::Client,
}

#[derive(Debug, Deserialize)]
pub struct InfoResponse {
    pub mode: String,
    pub vkey: String,
}

#[derive(Serialize)]
struct WorkRequest<'a> {
    prev_state: &'a State,
    batch: &'a Batch,
}

#[derive(Debug, Deserialize)]
pub struct ProveResponse {
    pub proof: String,
    pub public_values: String,
    /// Returned by prover-svc but not currently consulted: the sequencer
    /// pinned the program vkey at startup via /info, and the L1 contract's
    /// programVKey is immutable.
    #[allow(dead_code)]
    pub vkey: String,
    pub prev_root: String,
    pub new_root: String,
    pub batch_hash: String,
}

impl ProverClient {
    pub fn new(base: impl Into<String>) -> Self {
        let base = base.into();
        // Real Groth16 proving locally can take an hour-plus on consumer
        // hardware (load proving key, witness gen, MSM); the default reqwest
        // 30s timeout would cut us off well before the prover finishes.
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2 * 60 * 60))
            .build()
            .expect("build reqwest client");
        Self {
            base: base.trim_end_matches('/').to_string(),
            http,
        }
    }

    pub async fn info(&self) -> Result<InfoResponse> {
        Ok(self
            .http
            .get(format!("{}/info", self.base))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    pub async fn prove(&self, prev_state: &State, batch: &Batch) -> Result<ProveResponse> {
        let req = WorkRequest { prev_state, batch };
        let resp = self
            .http
            .post(format!("{}/prove", self.base))
            .json(&req)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("prover-svc /prove failed ({status}): {body}"));
        }
        Ok(resp.json().await?)
    }
}
