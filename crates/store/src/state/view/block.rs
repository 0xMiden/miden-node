//! Block header reads.

use miden_node_utils::tracing::miden_instrument;
use miden_protocol::block::{BlockHeader, BlockNumber};
use miden_protocol::crypto::merkle::mmr::MmrProof;

use super::StateView;
use crate::COMPONENT;
use crate::errors::GetBlockHeaderError;

impl StateView {
    /// Queries a [BlockHeader] from the database, and returns it alongside its inclusion proof.
    ///
    /// If [None] is given as the value of `block_num`, the data for the latest [BlockHeader] is
    /// returned.
    #[miden_instrument(
        level = "debug",
        target = COMPONENT,
        skip_all,
        err,
    )]
    pub async fn get_block_header(
        &self,
        block_num: Option<BlockNumber>,
        include_mmr_proof: bool,
    ) -> Result<(Option<BlockHeader>, Option<MmrProof>), GetBlockHeaderError> {
        // Resolve "latest" against the view's snapshot rather than the DB: mid-apply, the DB may
        // already contain a block that the snapshot's blockchain cannot prove yet. Scoping the DB
        // query by the view's tip keeps the header and MMR proof consistent.
        let latest_block_num = self.tip();
        let block_num = block_num.unwrap_or(latest_block_num);
        if block_num > latest_block_num {
            return Ok((None, None));
        }

        let block_header = self.db().select_block_header_by_block_num(Some(block_num)).await?;
        if let Some(header) = block_header {
            let mmr_proof = if include_mmr_proof {
                let mmr_proof = self.blockchain().open(header.block_num())?;
                Some(mmr_proof)
            } else {
                None
            };
            Ok((Some(header), mmr_proof))
        } else {
            Ok((None, None))
        }
    }
}
