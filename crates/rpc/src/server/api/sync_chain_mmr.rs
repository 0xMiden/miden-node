use miden_node_proto::generated as proto;
use miden_node_store::StateSyncError;
use miden_node_utils::tracing::miden_instrument;
use miden_protocol::block::BlockNumber;
use tonic::Status;
use tracing::debug;

use super::RpcService;
use crate::{COMPONENT, LOG_TARGET};

#[tonic::async_trait]
impl proto::server::rpc_api::SyncChainMmr for RpcService {
    type Input = proto::rpc::SyncChainMmrRequest;
    type Output = proto::rpc::SyncChainMmrResponse;

    fn decode(request: proto::rpc::SyncChainMmrRequest) -> tonic::Result<Self::Input> {
        Ok(request)
    }

    fn encode(output: Self::Output) -> tonic::Result<proto::rpc::SyncChainMmrResponse> {
        Ok(output)
    }

    #[miden_instrument(
        target = COMPONENT,
        name = "sync_chain_mmr",
        skip_all,
        fields(
            current_client_block_height = %request.current_client_block_height,
            finality_level = %request.finality_level().as_str_name(),
        ),
        err,
    )]
    async fn handle(
        &self,
        request: Self::Input,
        _metadata: &tonic::metadata::MetadataMap,
        _extensions: &tonic::codegen::http::Extensions,
    ) -> tonic::Result<Self::Output> {
        debug!(target: LOG_TARGET, "Syncing chain MMR");

        let current_client_block_height = BlockNumber::from(request.current_client_block_height);
        let finality_level = request.finality_level();
        let (block_range, sync_result) = self
            .state
            .with_view(async |view| {
                let sync_target = match finality_level {
                    proto::rpc::FinalityLevel::Committed
                    | proto::rpc::FinalityLevel::Unspecified => view.tip(),
                    // The proven tip is read from a watch channel, not the view's snapshot, so
                    // clamp it to the view's tip: a block could be committed and proven between
                    // taking the view and reading the proven tip, and the view cannot serve blocks
                    // beyond its snapshot.
                    proto::rpc::FinalityLevel::Proven => self.state.proven_tip().min(view.tip()),
                };
                let block_range = current_client_block_height..=sync_target;
                let result = if current_client_block_height > sync_target {
                    None
                } else {
                    Some(view.sync_chain_mmr(block_range.clone()).await)
                };
                (block_range, result)
            })
            .await;

        let Some(sync_result) = sync_result else {
            return Err(Status::invalid_argument(format!(
                "start block is not known: current client block height {current_client_block_height} is greater than chain tip {}",
                block_range.end(),
            )));
        };
        let (mmr_delta, block_header, block_signatures) = sync_result.map_err(|err| match err {
            StateSyncError::RangeBeyondTip(_) => Status::invalid_argument(err.to_string()),
            _ => Status::internal(err.to_string()),
        })?;

        Ok(proto::rpc::SyncChainMmrResponse {
            block_range: Some(proto::rpc::BlockRange {
                block_from: block_range.start().as_u32(),
                block_to: block_range.end().as_u32(),
            }),
            mmr_delta: Some(mmr_delta.into()),
            block_header: Some(block_header.into()),
            block_signatures: block_signatures.as_signatures().iter().map(Into::into).collect(),
        })
    }
}
