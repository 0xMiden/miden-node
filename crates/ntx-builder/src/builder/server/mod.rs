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
use miden_objects::{
    Digest,
    note::{Note, Nullifier},
};
use state::NtxBuilderState;
use tonic::{Request, Response, Status};
use tracing::info;

use crate::COMPONENT;

mod state;

#[derive(Debug)]
pub struct NtxBuilderApi {
    state: Arc<Mutex<NtxBuilderState>>,
}

impl NtxBuilderApi {
    pub async fn new(unconsumed_network_notes: Vec<Note>) -> Self {
        let state = NtxBuilderState::new(unconsumed_network_notes);
        Self { state: Arc::new(Mutex::new(state)) }
    }

    pub fn state(&self) -> Arc<Mutex<NtxBuilderState>> {
        self.state.clone()
    }
}

#[tonic::async_trait]
impl Api for NtxBuilderApi {
    async fn submit_network_notes(
        &self,
        request: Request<SubmitNetworkNotesRequest>,
    ) -> Result<Response<()>, Status> {
        let req = request.into_inner();
        let tx_id = req.transaction_id.ok_or_else(|| {
            Status::invalid_argument("transaction_id is required in SubmitNetworkNotesRequest")
        })?;

        info!(
            target: COMPONENT,
            tx_id = %tx_id,
            note_count = req.note.len(),
            "Received network notes"
        );

        let notes: Vec<Note> = try_convert(req.note)
            .map_err(|err| Status::invalid_argument(format!("invalid note list: {err}")))?;

        let mut state = self
            .state
            .lock()
            .map_err(|e| Status::internal(format!("Failed to lock state: {}", e)))?;

        state.add_unconsumed_notes(notes);

        Ok(Response::new(()))
    }

    async fn update_network_notes(
        &self,
        request: Request<UpdateNetworkNotesRequest>,
    ) -> Result<Response<()>, Status> {
        let request = request.into_inner();

        info!(
            target: COMPONENT,
            update_count = request.nullifiers.len(),
            "Received nullifier updates"
        );

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
            .map_err(|e| Status::internal(format!("Failed to lock state: {}", e)))?;

        state.discard_by_nullifiers(&nullifiers);

        Ok(Response::new(()))
    }

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
            .map_err(|e| Status::internal(format!("Failed to lock state: {}", e)))?;
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
                state.commit_transaction(tx_id.into());
            } else {
                state.discard_transaction(tx_id.into());
            }
        }
        Ok(Response::new(()))
    }
}
