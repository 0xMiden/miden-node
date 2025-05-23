use std::sync::{Arc, Mutex};

use miden_node_proto::{
    generated::{
        ntx_builder::api_server::Api,
        requests::{
            SubmitNetworkNotesRequest, UpdateNetworkNotesRequest, UpdateTransactionStatusRequest,
        },
        transaction::TransactionStatus,
    },
    try_convert,
};
use miden_node_utils::network_note::NetworkNote;
use miden_objects::{Digest, note::Nullifier};
use state::PendingNotes;
use tonic::{Request, Response, Status};
use tracing::{info, instrument};

use crate::COMPONENT;

mod state;

#[derive(Debug)]
pub struct NtxBuilderApi {
    state: Arc<Mutex<PendingNotes>>,
}

impl NtxBuilderApi {
    pub fn new(unconsumed_network_notes: Vec<NetworkNote>) -> Self {
        let state = PendingNotes::new(unconsumed_network_notes);
        Self { state: Arc::new(Mutex::new(state)) }
    }

    pub fn state(&self) -> Arc<Mutex<PendingNotes>> {
        self.state.clone()
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

        let mut state = self
            .state
            .lock()
            .map_err(|e| Status::internal(format!("Failed to lock state: {e}")))?;

        state.queue_unconsumed_notes(notes);

        Ok(Response::new(()))
    }

    #[instrument(target = COMPONENT, name = "ntx_builder.update_network_notes", skip_all, err)]
    async fn update_network_notes(
        &self,
        request: Request<UpdateNetworkNotesRequest>,
    ) -> Result<Response<()>, Status> {
        let request = request.into_inner();

        let nullifiers: Vec<Nullifier> = request
            .nullifiers
            .into_iter()
            .map(Digest::try_from)
            .map(|res| res.map(Nullifier::from))
            .collect::<Result<_, _>>()
            .map_err(|err| {
                Status::invalid_argument(format!("error when convertinf input nullifiers: {err}"))
            })?;

        let mut state = self
            .state
            .lock()
            .map_err(|e| Status::internal(format!("failed to lock state: {e}")))?;

        state.discard_by_nullifiers(&nullifiers);

        Ok(Response::new(()))
    }

    #[instrument(target = COMPONENT, name = "ntx_builder.update_transaction_status", skip_all, err)]
    async fn update_transaction_status(
        &self,
        request: Request<UpdateTransactionStatusRequest>,
    ) -> Result<Response<()>, Status> {
        let request = request.into_inner();

        info!(
            target: COMPONENT,
            update_count = request.updates.len(),
            "Received transaction status updates"
        );

        let mut state = self
            .state
            .lock()
            .map_err(|e| Status::internal(format!("failed to lock state: {e}")))?;
        for tx in request.updates {
            let tx_id: Digest = tx
                .transaction_id
                .ok_or(Status::not_found("transaction ID not found in request"))?
                .try_into()
                .map_err(|err| {
                    Status::invalid_argument(format!(
                        "transaction ID from request is not valid: {err}"
                    ))
                })?;

            if TransactionStatus::Commited == tx.status() {
                state.commit_inflight(tx_id.into());
            } else {
                state.rollback_inflight(tx_id.into());
            }
        }
        Ok(Response::new(()))
    }
}
