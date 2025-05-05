use axum::{
    extract::{Query, State},
    http::{Response, StatusCode},
    response::IntoResponse,
};
use http::header;
use http_body_util::Full;
use miden_node_utils::errors::ErrorReport;
use miden_objects::{
    AccountIdError,
    account::AccountId,
    block::BlockNumber,
    note::{Note, NoteDetails, NoteExecutionMode, NoteFile, NoteId, NoteTag},
    utils::serde::Serializable,
};
use serde::Deserialize;
use tokio::sync::{mpsc::error::TrySendError, oneshot};
use tonic::body;

use crate::{
    faucet::MintRequest,
    types::{AssetOptions, NoteType},
};

type RequestSender = tokio::sync::mpsc::Sender<(MintRequest, oneshot::Sender<(BlockNumber, Note)>)>;

#[derive(Clone)]
pub struct GetTokensState {
    request_sender: RequestSender,
    asset_options: AssetOptions,
}

impl GetTokensState {
    pub fn new(request_sender: RequestSender, asset_options: AssetOptions) -> Self {
        Self { request_sender, asset_options }
    }
}

/// Used to receive the initial request from the user.
///
/// Further parsing is done to get the expected [`MintRequest`] expected by the faucet client.
#[derive(Deserialize)]
pub struct RawMintRequest {
    pub account_id: String,
    pub is_private_note: bool,
    pub asset_amount: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum InvalidRequest {
    #[error("account ID failed to parse")]
    AccountId(#[source] AccountIdError),
    #[error("asset amount {0} is not one of the provided options")]
    AssetAmount(u64),
}

pub enum GetTokenError {
    InvalidRequest(InvalidRequest),
    FaucetOverloaded,
    FaucetClosed,
    FaucetReturnChannelClosed,
    ResponseBuilder(http::Error),
}

impl GetTokenError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            Self::FaucetOverloaded | Self::FaucetClosed => StatusCode::SERVICE_UNAVAILABLE,
            Self::FaucetReturnChannelClosed | Self::ResponseBuilder(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            },
        }
    }

    /// Take care to not expose internal errors here.
    fn user_facing_error(&self) -> String {
        match self {
            Self::InvalidRequest(invalid_request) => invalid_request.as_report(),
            Self::FaucetOverloaded => {
                "The faucet is currently overloaded, please try again later".to_owned()
            },
            Self::FaucetClosed => {
                "The faucet is currently unavailable, please try again later".to_owned()
            },
            Self::FaucetReturnChannelClosed | Self::ResponseBuilder(_) => {
                "Internal error".to_owned()
            },
        }
    }

    /// Write a trace log for the error, if applicable.
    fn trace(&self) {
        match self {
            Self::InvalidRequest(_) => {},
            Self::FaucetOverloaded => tracing::warn!("faucet client is overloaded"),
            Self::FaucetClosed => {
                tracing::error!("faucet channel is closed but requests are still coming in");
            },
            Self::FaucetReturnChannelClosed => {
                tracing::error!("result channel from the faucet closed mid-request");
            },
            Self::ResponseBuilder(error) => {
                tracing::error!(error = error.as_report(), "failed to build response");
            },
        }
    }
}

impl IntoResponse for GetTokenError {
    fn into_response(self) -> axum::response::Response {
        // TODO: This is a hacky way of doing error logging, but
        // its one of the last times we have the error before
        // it becomes opaque. Should replace this by something
        // better.
        self.trace();

        (self.status_code(), self.user_facing_error()).into_response()
    }
}

impl RawMintRequest {
    /// Further validates a raw request, turning it into a valid [`MintRequest`] which can be
    /// submitted to the faucet client.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    ///   - the account ID is not a valid hex string
    ///   - the asset amount is not one of the provided options
    fn validate(self, options: &AssetOptions) -> Result<MintRequest, InvalidRequest> {
        let note_type = if self.is_private_note {
            NoteType::Private
        } else {
            NoteType::Public
        };

        let account_id =
            AccountId::from_hex(&self.account_id).map_err(InvalidRequest::AccountId)?;
        let asset_amount = options
            .validate(self.asset_amount)
            .ok_or(InvalidRequest::AssetAmount(self.asset_amount))?;

        Ok(MintRequest { account_id, note_type, asset_amount })
    }
}

pub async fn get_tokens(
    State(state): State<GetTokensState>,
    Query(request): Query<RawMintRequest>,
) -> Result<impl IntoResponse, GetTokenError> {
    let request = request.validate(&state.asset_options).map_err(GetTokenError::InvalidRequest)?;
    let request_account = request.account_id;

    // Fill in the request's tracing fields.
    //
    // These were registered in the trace layer in the router.
    let span = tracing::Span::current();
    span.record("account", request.account_id.to_hex());
    span.record("amount", request.asset_amount.inner());
    span.record("note_type", request.note_type.to_string());

    // Submit the request to the client and wait for the result.
    let (tx_result, rx_result) = oneshot::channel();
    state.request_sender.try_send((request, tx_result)).map_err(|err| match err {
        TrySendError::Full(_) => GetTokenError::FaucetOverloaded,
        TrySendError::Closed(_) => GetTokenError::FaucetClosed,
    })?;

    let (block_height, note) =
        rx_result.await.map_err(|_| GetTokenError::FaucetReturnChannelClosed)?;

    let note_id: NoteId = note.id();
    let note_details = NoteDetails::new(note.assets().clone(), note.recipient().clone());
    // SAFETY: NoteTag creation can only error for network execution mode, and we only use private
    // or public.
    let note_tag = NoteTag::from_account_id(request_account, NoteExecutionMode::Local).unwrap();

    let bytes = NoteFile::NoteDetails {
        details: note_details,
        after_block_num: block_height,
        tag: Some(note_tag),
    }
    .to_bytes();

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_DISPOSITION, "attachment; filename=note.mno")
        .header("Note-Id", note_id.to_string())
        .body(body::boxed(Full::from(bytes)))
        .map_err(GetTokenError::ResponseBuilder)
}
