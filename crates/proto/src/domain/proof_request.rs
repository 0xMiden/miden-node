// PROOF REQUEST
// ================================================================================================

use miden_objects::batch::OrderedBatches;
use miden_objects::block::{BlockHeader, BlockInputs};
use miden_objects::utils::{
    ByteReader,
    ByteWriter,
    Deserializable,
    DeserializationError,
    Serializable,
};

pub struct BlockProofRequest {
    pub tx_batches: OrderedBatches,
    pub block_header: BlockHeader,
    pub block_inputs: BlockInputs,
}

impl Serializable for BlockProofRequest {
    fn write_into<W: ByteWriter>(&self, target: &mut W) {
        self.tx_batches.write_into(target);
        self.block_header.write_into(target);
        // TODO(serge): Add serde for block inputs
        // self.block_inputs.write_into(target);
    }
}

impl Deserializable for BlockProofRequest {
    fn read_from<R: ByteReader>(source: &mut R) -> Result<Self, DeserializationError> {
        todo!()
        //let block = Self {
        //    tx_batches: OrderedBatches::read_from(source)?,
        //    block_header: BlockHeader::read_from(source)?,
        //    // TODO(serge): Add serde for block inputs
        //    block_inputs: BlockInputs::read_from(source)?,
        //};

        //Ok(block)
    }
}
