use miden_node_proto::generated::{
    ntx_builder::api_server::Api as GeneratedApi,
    requests::{
        SubmitNetworkNotesRequest, UpdateTransactionStatusRequest,
        update_transaction_status_request::TransactionUpdate as ProtoTxUpdate,
    },
    transaction::TransactionStatus,
};
use miden_objects::transaction::TransactionId;

pub(crate) enum BlockProducerEvent {
    TxStateUpdates(Vec<(TransactionId, TransactionStatus)>),
}

pub(crate) struct GrpcServer {
    events: tokio::sync::mpsc::Sender<BlockProducerEvent>,
}

#[tonic::async_trait]
impl GeneratedApi for GrpcServer {
    /// Submit a list of network notes to the network transaction builder.
    async fn submit_network_notes(
        &self,
        request: tonic::Request<SubmitNetworkNotesRequest>,
    ) -> Result<tonic::Response<()>, tonic::Status> {
        self.try_send_event(request.into_inner()).unwrap();

        Ok(tonic::Response::new(()))
    }
    /// Update network transaction builder with transaction status changes.
    async fn update_transaction_status(
        &self,
        request: tonic::Request<UpdateTransactionStatusRequest>,
    ) -> Result<tonic::Response<()>, tonic::Status> {
        self.try_send_event(request.into_inner()).unwrap();

        Ok(tonic::Response::new(()))
    }
}

impl GrpcServer {
    fn try_send_event(&self, event: impl Into<BlockProducerEvent>) -> Result<(), ()> {
        match self.events.try_send(event.into()) {
            Ok(_) => todo!(),
            Err(_) => todo!(),
        }
    }
}

impl TryFrom<UpdateTransactionStatusRequest> for BlockProducerEvent {
    type Error = miden_node_proto::errors::ConversionError;

    fn try_from(value: UpdateTransactionStatusRequest) -> Self {
        Ok(Self::TxStateUpdates(
            value
                .updates
                .into_iter()
                .map(|ProtoTxUpdate { transaction_id, status }| (todo!(), status))
                .collect()?,
        ))
    }
}

impl TryFrom<SubmitNetworkNotesRequest> for BlockProducerEvent {
    type Error = miden_node_proto::errors::ConversionError;

    fn try_from(value: SubmitNetworkNotesRequest) -> Result<Self, Self::Error> {
        Ok(Self {})
    }
}
