use miden_node_proto::generated as proto;
use miden_node_utils::grpc::ClientIp;
use miden_node_utils::tracing::miden_instrument;
use miden_protocol::block::{BlockNumber, BlockProof};
use miden_protocol::utils::serde::Deserializable;
use tracing::debug;

use super::super::{COMPONENT, RpcService};
use super::stream::{StreamItem, SubscriptionStream};
use crate::LOG_TARGET;

#[tonic::async_trait]
impl proto::server::rpc_api::ProofSubscription for RpcService {
    type Input = BlockNumber;
    type Item = StreamItem;
    type ItemStream = SubscriptionStream;

    fn decode(request: proto::rpc::ProofSubscriptionRequest) -> tonic::Result<Self::Input> {
        Ok(BlockNumber::from(request.block_from))
    }

    fn encode(event: Self::Item) -> tonic::Result<proto::rpc::ProofSubscriptionResponse> {
        if !event.data.is_empty() {
            return Err(tonic::Status::data_loss(format!(
                "stored proof for block {} uses an unsupported non-empty placeholder encoding",
                event.block
            )));
        }
        let block_proof = BlockProof::read_from_bytes(&event.data).map_err(|err| {
            tonic::Status::data_loss(format!(
                "stored proof for block {} could not be decoded: {err}",
                event.block
            ))
        })?;
        Ok(proto::rpc::ProofSubscriptionResponse {
            block_num: event.block.as_u32(),
            proven_chain_tip: event.tip.as_u32(),
            block_proof: Some(block_proof.into()),
        })
    }

    #[miden_instrument(
        target = COMPONENT,
        name = "proof_subscription",
        fields(
            block.from = %input,
        ),
        err,
    )]
    async fn handle(
        &self,
        input: Self::Input,
        _metadata: &tonic::metadata::MetadataMap,
        extensions: &tonic::codegen::http::Extensions,
    ) -> tonic::Result<Self::ItemStream> {
        let client_ip = ClientIp::from_extensions(extensions);

        debug!(target: LOG_TARGET, "Subscribing to block proofs");

        let from = input;
        SubscriptionStream::proofs(self, from, client_ip)
    }
}

#[cfg(test)]
mod tests {
    use miden_protocol::block::BlockNumber;
    use tonic::Code;

    use super::{RpcService, StreamItem};
    use crate::server::rpc_api::ProofSubscription;

    #[test]
    fn corrupt_stored_proof_is_reported_as_data_loss() {
        let result = <RpcService as ProofSubscription>::encode(StreamItem {
            data: vec![0xff],
            block: BlockNumber::from(4_u32),
            tip: BlockNumber::from(7_u32),
        });

        assert_eq!(result.unwrap_err().code(), Code::DataLoss);
    }
}
