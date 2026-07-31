//! Block and proof serving for replica subscriptions.
//!
//! Replica subscribers stream committed blocks and block proofs from this node. The writer pushes
//! each freshly committed block (and the proof path each proven block) into a FIFO cache here, so
//! subscribers keeping up with the tip are served from memory; a subscriber that has fallen
//! behind the cache window falls back to reading the block store.
//!
//! These reads serve raw block/proof bytes from the caches and the block store — they never touch
//! the database or tree state, which is why they live on [`State`] directly rather than on a
//! [`StateView`](super::StateView). The tip checks gating them are live availability checks
//! ("is this block committed/proven yet?"), deliberately race-tolerant: a `None` for a block that
//! commits an instant later is corrected by the next tip-watch wakeup.

use std::sync::Arc;

use miden_node_utils::block_cache::BlockOrderedCache;
use miden_protocol::block::BlockNumber;

use crate::errors::DatabaseError;
use crate::state::State;

// BLOCK NOTIFICATION
// ================================================================================================

/// A committed block notification stored in the [`BlockCache`].
#[derive(Clone, Debug)]
pub struct BlockNotification(Arc<Block>);

impl BlockNotification {
    pub fn new(block_num: BlockNumber, block_bytes: Vec<u8>) -> Self {
        Self(Arc::new(Block { block_num, block_bytes }))
    }

    pub fn block_num(&self) -> BlockNumber {
        self.0.block_num
    }

    pub fn block_bytes(&self) -> &[u8] {
        &self.0.block_bytes
    }
}

#[derive(Clone, Debug)]
struct Block {
    pub block_num: BlockNumber,
    pub block_bytes: Vec<u8>,
}

// PROOF NOTIFICATION
// ================================================================================================

/// A proven block notification stored in the [`ProofCache`].
#[derive(Clone, Debug)]
pub struct ProofNotification(Arc<Proof>);

impl ProofNotification {
    pub fn new(block_num: BlockNumber, proof_bytes: Vec<u8>) -> Self {
        Self(Arc::new(Proof { block_num, proof_bytes }))
    }

    pub fn block_num(&self) -> BlockNumber {
        self.0.block_num
    }

    pub fn proof_bytes(&self) -> &[u8] {
        &self.0.proof_bytes
    }
}

#[derive(Clone, Debug)]
struct Proof {
    block_num: BlockNumber,
    proof_bytes: Vec<u8>,
}

// CACHES
// ================================================================================================

/// FIFO cache of recent committed blocks for replica subscriptions.
pub type BlockCache = BlockOrderedCache<BlockNotification>;

/// FIFO cache of recent block proofs for replica subscriptions.
pub type ProofCache = BlockOrderedCache<ProofNotification>;

// REPLICA STATE ACCESS
// ================================================================================================

impl State {
    /// Loads a block from the in-memory replica cache or block store. Return `Ok(None)` if the
    /// block is not found.
    pub async fn load_block(
        &self,
        block_num: BlockNumber,
    ) -> Result<Option<Vec<u8>>, DatabaseError> {
        if block_num > self.committed_tip() {
            return Ok(None);
        }
        if let Some(block) = self.block_cache.get(block_num) {
            return Ok(Some(block.block_bytes().to_vec()));
        }
        self.block_store.load_block(block_num).await.map_err(Into::into)
    }

    /// Loads a block proof from the in-memory replica cache or block store. Returns `Ok(None)` if
    /// the proof is not found.
    pub async fn load_proof(
        &self,
        block_num: BlockNumber,
    ) -> Result<Option<Vec<u8>>, DatabaseError> {
        if block_num > self.proven_tip() {
            return Ok(None);
        }
        if let Some(proof) = self.proof_cache.get(block_num) {
            return Ok(Some(proof.proof_bytes().to_vec()));
        }
        self.block_store.load_proof(block_num).await.map_err(Into::into)
    }

    /// Loads serialized block proving inputs from the block store.
    pub async fn load_proving_inputs(
        &self,
        block_num: BlockNumber,
    ) -> std::io::Result<Option<Vec<u8>>> {
        self.block_store.load_proving_inputs(block_num).await
    }
}
