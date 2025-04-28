use miden_node_proto::generated::{
    ntx_builder::api_server::Api as GeneratedApi,
    requests::{SubmitNetworkNotesRequest, UpdateTransactionStatusRequest},
};

pub(crate) struct Server;

#[tonic::async_trait]
impl GeneratedApi for Server {
    /// Submit a list of network notes to the network transaction builder.
    async fn submit_network_notes(
        &self,
        request: tonic::Request<SubmitNetworkNotesRequest>,
    ) -> Result<tonic::Response<()>, tonic::Status> {
        todo!();
    }
    /// Update network transaction builder with transaction status changes.
    async fn update_transaction_status(
        &self,
        request: tonic::Request<UpdateTransactionStatusRequest>,
    ) -> Result<tonic::Response<()>, tonic::Status> {
        todo!()
    }
}
