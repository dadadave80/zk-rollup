use anyhow::{anyhow, Result};
use shared_types::{Batch, Hash, State, Tx};
use stf::{apply_tx, StfError};
use tokio::sync::RwLock;

/// Source-of-truth in-memory state for the sequencer.
///
/// Two layered states:
/// - `canonical_state` mirrors what the L1 contract has accepted; it advances
///   only after `submitBatch` confirms.
/// - `speculative_state` is `canonical_state` with all unbatched mempool txs
///   applied. New `/tx` requests are validated against it so back-to-back txs
///   from the same sender increment nonces and decrement balances correctly.
pub struct SequencerState {
    pub canonical_state: State,
    pub canonical_root: Hash,
    pub speculative_state: State,
    pub mempool: Vec<Tx>,
    pub batch_count: u64,
}

impl SequencerState {
    pub fn from_genesis(state: State) -> Self {
        let canonical_root = state.merkle_root();
        Self {
            speculative_state: state.clone(),
            canonical_state: state,
            canonical_root,
            mempool: Vec::new(),
            batch_count: 0,
        }
    }

    /// Apply a tx speculatively; on success push it to the mempool. On failure
    /// the speculative state is unchanged.
    pub fn admit_tx(&mut self, tx: Tx) -> Result<(), StfError> {
        // apply_tx mutates state in place; clone, try, then commit on success.
        let mut next = self.speculative_state.clone();
        apply_tx(&mut next, &tx)?;
        self.speculative_state = next;
        self.mempool.push(tx);
        Ok(())
    }

    /// Drain the mempool into a Batch and snapshot the canonical state at that
    /// instant. Used as the prove input.
    pub fn drain_for_batch(&mut self) -> (State, Batch) {
        let prev = self.canonical_state.clone();
        let txs = std::mem::take(&mut self.mempool);
        // Reset speculative — until the batch commits, the mempool view
        // collapses back to canonical.
        self.speculative_state = self.canonical_state.clone();
        (prev, Batch { txs })
    }

    /// Restore mempool after a failed batch (e.g. prover error or L1 revert),
    /// so we don't lose user txs. Re-runs them speculatively in original order;
    /// any tx that no longer applies is dropped.
    pub fn restore_mempool(&mut self, batch: Batch) {
        let txs = batch.txs;
        for tx in txs {
            if self.admit_tx(tx).is_err() {
                // Silently drop; the user's nonce already moved on or balance
                // is gone. They can resubmit if needed.
            }
        }
    }

    /// After a batch is settled on L1 we advance canonical state to match what
    /// the proof committed. We re-run apply_tx locally for each tx since the
    /// proof already certifies the same arithmetic.
    pub fn commit_batch(&mut self, batch: &Batch, new_root: Hash) -> Result<()> {
        for tx in &batch.txs {
            apply_tx(&mut self.canonical_state, tx)
                .map_err(|e| anyhow!("post-settlement re-apply diverged: {e:?}"))?;
        }
        let recomputed = self.canonical_state.merkle_root();
        if recomputed != new_root {
            return Err(anyhow!(
                "post-settlement root mismatch: local {} vs L1 {}",
                hex::encode(recomputed),
                hex::encode(new_root)
            ));
        }
        self.canonical_root = new_root;
        self.batch_count += 1;
        // speculative was reset in drain_for_batch; nothing else to do.
        Ok(())
    }
}

pub type SharedState = std::sync::Arc<RwLock<SequencerState>>;
