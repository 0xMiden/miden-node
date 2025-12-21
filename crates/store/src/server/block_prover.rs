use miden_block_prover::LocalBlockProver;
use miden_objects::MIN_PROOF_SECURITY_LEVEL;
use miden_objects::batch::OrderedBatches;
use miden_objects::block::{BlockHeader, BlockInputs, BlockProof};
use miden_remote_prover_client::remote_prover::block_prover::RemoteBlockProver;
use tracing::{info, instrument};

use crate::COMPONENT;
use crate::server::api::StoreProverError;

// BLOCK PROVER
// ================================================================================================

/// Block prover which allows for proving via either local or remote backend.
///
/// The local proving variant is intended for development and testing purposes.
/// The remote proving variant is intended for production use.
pub enum BlockProver {
    Local(LocalBlockProver),
    Remote(RemoteBlockProver),
}

impl BlockProver {
    pub fn new_local(security_level: Option<u32>) -> Self {
        info!(target: COMPONENT, "Using local block prover");
        let security_level = security_level.unwrap_or(MIN_PROOF_SECURITY_LEVEL);
        Self::Local(LocalBlockProver::new(security_level))
    }

    pub fn new_remote(endpoint: impl Into<String>) -> Self {
        info!(target: COMPONENT, "Using remote block prover");
        Self::Remote(RemoteBlockProver::new(endpoint))
    }

    #[instrument(target = COMPONENT, skip_all, err)]
    pub async fn prove(
        &self,
        tx_batches: OrderedBatches,
        block_header: BlockHeader,
        block_inputs: BlockInputs,
    ) -> Result<BlockProof, StoreProverError> {
        match self {
            Self::Local(prover) => Ok(prover.prove(tx_batches, block_header, block_inputs)?),
            Self::Remote(prover) => {
                Ok(prover.prove(tx_batches, block_header, block_inputs).await?)
            },
        }
    }
}
