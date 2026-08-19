use miden_node_proto::generated as proto;
use miden_node_utils::grpc::ClientIp;
use miden_node_utils::tracing::miden_instrument;
use miden_protocol::block::{BlockNumber, SignedBlock};
use miden_protocol::utils::serde::Deserializable;
use tracing::debug;

use super::super::{COMPONENT, RpcService};
use super::stream::{StreamItem, SubscriptionStream};
use crate::LOG_TARGET;

#[tonic::async_trait]
impl proto::server::rpc_api::BlockSubscription for RpcService {
    type Input = BlockNumber;
    type Item = StreamItem;
    type ItemStream = SubscriptionStream;

    fn decode(request: proto::rpc::BlockSubscriptionRequest) -> tonic::Result<Self::Input> {
        Ok(BlockNumber::from(request.block_from))
    }

    fn encode(event: Self::Item) -> tonic::Result<proto::rpc::BlockSubscriptionResponse> {
        let signed_block = SignedBlock::read_from_bytes(&event.data).map_err(|err| {
            tonic::Status::data_loss(format!(
                "stored block {} could not be decoded: {err}",
                event.block
            ))
        })?;
        Ok(proto::rpc::BlockSubscriptionResponse {
            committed_chain_tip: event.tip.as_u32(),
            signed_block: Some(signed_block.into()),
        })
    }

    #[miden_instrument(
        target = COMPONENT,
        name = "block_subscription",
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

        debug!(target: LOG_TARGET, "Subscribing to blocks");

        let from = input;
        SubscriptionStream::blocks(self, from, client_ip)
    }
}

#[cfg(test)]
mod tests {
    use miden_protocol::block::BlockNumber;
    use tonic::Code;

    use super::{RpcService, StreamItem};
    use crate::server::rpc_api::BlockSubscription;

    #[test]
    fn corrupt_stored_block_is_reported_as_data_loss() {
        let result = <RpcService as BlockSubscription>::encode(StreamItem {
            data: vec![0xff],
            block: BlockNumber::from(4_u32),
            tip: BlockNumber::from(7_u32),
        });

        assert_eq!(result.unwrap_err().code(), Code::DataLoss);
    }
}
