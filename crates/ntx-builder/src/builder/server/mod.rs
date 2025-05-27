use miden_node_proto::{
    domain::note::NetworkNote,
    generated::{
        ntx_builder::api_server::Api,
        requests::{
            SubmitNetworkNotesRequest, UpdateNetworkNotesRequest, UpdateTransactionStatusRequest,
        },
        transaction::TransactionStatus,
    },
    try_convert,
};
use miden_objects::{Digest, note::Nullifier, transaction::TransactionId};
use tonic::{Request, Response, Status};
use tracing::{info, instrument};

use crate::COMPONENT;

mod state;
pub use state::PendingNotes;

use super::SharedPendingNotes;

#[derive(Debug)]
pub struct NtxBuilderApi {
    state: SharedPendingNotes,
}

impl NtxBuilderApi {
    pub fn new(notes_queue: SharedPendingNotes) -> Self {
        Self { state: notes_queue }
    }
}

#[tonic::async_trait]
impl Api for NtxBuilderApi {
    #[instrument(target = COMPONENT, name = "ntx_builder.submit_network_notes", skip_all, err)]
    async fn submit_network_notes(
        &self,
        request: Request<SubmitNetworkNotesRequest>,
    ) -> Result<Response<()>, Status> {
        let req = request.into_inner();

        let notes: Vec<NetworkNote> = try_convert(req.note)
            .map_err(|err| Status::invalid_argument(format!("invalid note list: {err}")))?;

        let mut state = self.state.lock().await;

        state.queue_unconsumed_notes(notes);

        Ok(Response::new(()))
    }

    #[instrument(target = COMPONENT, name = "ntx_builder.update_network_notes", skip_all, err)]
    async fn update_network_notes(
        &self,
        request: Request<UpdateNetworkNotesRequest>,
    ) -> Result<Response<()>, Status> {
        let request = request.into_inner();

        let tx_id = request
            .transaction_id
            .map(TransactionId::try_from)
            .ok_or(Status::not_found("transaction ID not found in request"))?
            .map_err(|err| Status::invalid_argument(format!("invalid transaction ID: {err}")))?;

        let nullifiers: Vec<Nullifier> = request
            .nullifiers
            .into_iter()
            .map(Digest::try_from)
            .map(|res| res.map(Nullifier::from))
            .collect::<Result<_, _>>()
            .map_err(|err| {
                Status::invalid_argument(format!("error when converting input nullifiers: {err}"))
            })?;

        let mut state = self.state.lock().await;

        state.insert_inflight(tx_id, nullifiers);

        Ok(Response::new(()))
    }

    #[instrument(target = COMPONENT, name = "ntx_builder.update_transaction_status", skip_all, err)]
    async fn update_transaction_status(
        &self,
        request: Request<UpdateTransactionStatusRequest>,
    ) -> Result<Response<()>, Status> {
        let request = request.into_inner();

        let mut state = self.state.lock().await;

        for tx in request.updates {
            let tx_id: TransactionId = tx
                .transaction_id
                .ok_or(Status::not_found("transaction ID not found in request"))?
                .try_into()
                .map_err(|err| {
                    Status::invalid_argument(format!(
                        "transaction ID from request is not valid: {err}"
                    ))
                })?;

            match tx.status() {
                TransactionStatus::Committed => {
                    let n = state.commit_inflight(tx_id);
                    info!(
                        target: COMPONENT,
                        committed = n,
                        tx_id = tx_id.to_hex(),
                        "Committed notes notes for transaction"
                    );
                },
                TransactionStatus::Reverted => {
                    let n = state.rollback_inflight(tx_id);
                    info!(
                        target: COMPONENT,
                        rolled_back = n,
                        tx_id = tx_id.to_hex(),
                        "Rolled back inflight notes notes after transaction got discarded"
                    );
                },
            }
        }
        Ok(Response::new(()))
    }
}
