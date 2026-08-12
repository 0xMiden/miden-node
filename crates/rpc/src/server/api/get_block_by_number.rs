use miden_node_proto::generated as proto;
use miden_node_utils::tracing::miden_instrument;
use miden_protocol::block::{BlockNumber, BlockProof, SignedBlock};
use miden_protocol::utils::serde::Deserializable;
use tracing::debug;

use super::{RpcService, database_error_to_status};
use crate::{COMPONENT, LOG_TARGET};

#[tonic::async_trait]
impl proto::server::rpc_api::GetBlockByNumber for RpcService {
    type Input = proto::blockchain::BlockRequest;
    type Output = proto::blockchain::MaybeBlock;

    fn decode(request: proto::blockchain::BlockRequest) -> tonic::Result<Self::Input> {
        Ok(request)
    }

    fn encode(output: Self::Output) -> tonic::Result<proto::blockchain::MaybeBlock> {
        Ok(output)
    }

    #[miden_instrument(
        target = COMPONENT,
        name = "get_block_by_number",
        fields(
            block.number = %request.block_num,
        ),
        err,
    )]
    async fn handle(
        &self,
        request: Self::Input,
        _metadata: &tonic::metadata::MetadataMap,
        _extensions: &tonic::codegen::http::Extensions,
    ) -> tonic::Result<Self::Output> {
        debug!(target: LOG_TARGET, ?request, "Getting block by number");

        let block_num = BlockNumber::from(request.block_num);
        let block = self
            .state
            .load_block(block_num)
            .await
            .map_err(|err| database_error_to_status(&err))?;
        let proof = if request.include_proof.unwrap_or_default() {
            self.state
                .load_proof(block_num)
                .await
                .map_err(|err| database_error_to_status(&err))?
        } else {
            None
        };

        let signed_block = block
            .map(|bytes| {
                SignedBlock::read_from_bytes(&bytes).map(Into::into).map_err(|err| {
                    tonic::Status::data_loss(format!(
                        "stored block {block_num} could not be decoded: {err}"
                    ))
                })
            })
            .transpose()?;
        let block_proof = proof
            .map(|bytes| {
                if !bytes.is_empty() {
                    return Err(tonic::Status::data_loss(format!(
                        "stored proof for block {block_num} uses an unsupported non-empty placeholder encoding"
                    )));
                }
                BlockProof::read_from_bytes(&bytes).map(Into::into).map_err(|err| {
                    tonic::Status::data_loss(format!(
                        "stored proof for block {block_num} could not be decoded: {err}"
                    ))
                })
            })
            .transpose()?;

        Ok(proto::blockchain::MaybeBlock { signed_block, block_proof })
    }
}
