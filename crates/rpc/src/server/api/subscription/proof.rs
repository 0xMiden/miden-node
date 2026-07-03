use miden_node_proto::generated as proto;
use miden_node_utils::grpc::ClientIp;
use miden_node_utils::tracing::miden_instrument;
use miden_protocol::block::BlockNumber;
use tonic::Request;
use tracing::debug;

use super::super::{COMPONENT, RpcService};
use super::stream::{StreamItem, SubscriptionStream};
use crate::LOG_TARGET;

pub struct ProofSubscriptionInput {
    request: proto::rpc::ProofSubscriptionRequest,
}

#[tonic::async_trait]
impl proto::server::rpc_api::ProofSubscription for RpcService {
    type Input = ProofSubscriptionInput;
    type Item = StreamItem;
    type ItemStream = SubscriptionStream;

    fn decode(request: proto::rpc::ProofSubscriptionRequest) -> tonic::Result<Self::Input> {
        Ok(ProofSubscriptionInput { request })
    }

    fn encode(event: Self::Item) -> tonic::Result<proto::rpc::ProofSubscriptionResponse> {
        Ok(proto::rpc::ProofSubscriptionResponse {
            block_num: event.block.as_u32(),
            proof: event.data,
            proven_chain_tip: event.tip.as_u32(),
        })
    }

    #[miden_instrument(
        target = COMPONENT,
        name = "proof_subscription",
        skip_all,
        fields(
            block.from = %input.request.block_from,
        ),
        err,
    )]
    async fn handle(
        &self,
        request_context: &Request<()>,
        input: Self::Input,
    ) -> tonic::Result<Self::ItemStream> {
        let ProofSubscriptionInput { request } = input;
        let client_ip = ClientIp::from_request(request_context);

        debug!(target: LOG_TARGET, "Subscribing to block proofs");

        let from = BlockNumber::from(request.block_from);

        SubscriptionStream::proofs(self, from, client_ip)
    }
}
