use axum::{
    Json,
    extract::State,
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
    client::MintRequest,
    errors::{MintError, MintResult},
    types::{AssetOptions, NoteType},
};

type RequestSender =
    tokio::sync::mpsc::Sender<(MintRequest, oneshot::Sender<MintResult<(BlockNumber, Note)>>)>;

pub struct GetTokensState {
    request_sender: RequestSender,
    asset_options: AssetOptions,
}

/// Used to receive the initial request from the user.
///
/// Further parsing is done to get the expected [`MintRequest`] expected by the faucet client.
#[derive(Deserialize)]
struct RawMintRequest {
    account_id: String,
    is_private_note: bool,
    asset_amount: u64,
}

#[derive(Debug, thiserror::Error)]
enum InvalidRequest {
    #[error("account ID failed to parse")]
    AccountId(AccountIdError),
    #[error("asset amount {0} is not one of the provided options")]
    AssetAmount(u64),
}

enum GetTokenError {
    InvalidRequest(InvalidRequest),
    ClientOverloaded,
    ClientClosed,
    ClientReturnChannelClosed,
    MintFailed(MintError),
    ResponseBuilder(http::Error),
}

impl GetTokenError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            Self::ClientOverloaded => StatusCode::SERVICE_UNAVAILABLE,
            Self::ClientClosed => StatusCode::SERVICE_UNAVAILABLE,
            Self::ClientReturnChannelClosed => StatusCode::INTERNAL_SERVER_ERROR,
            Self::MintFailed(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::ResponseBuilder(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Take care to not expose internal errors here.
    fn user_facing_error(&self) -> String {
        match self {
            Self::InvalidRequest(invalid_request) => invalid_request.as_report(),
            Self::ClientOverloaded => {
                "The faucet is currently overloaded, please try again later".to_owned()
            },
            Self::ClientClosed => {
                "The faucet is currently unavailable, please try again later".to_owned()
            },
            Self::ClientReturnChannelClosed | Self::ResponseBuilder(_) | Self::MintFailed(_) => {
                "Internal error".to_owned()
            },
        }
    }

    /// Write a trace log for the error, if applicable.
    fn trace(&self) {
        match self {
            Self::InvalidRequest(_) => {},
            Self::ClientOverloaded => tracing::warn!("faucet client is overloaded"),
            Self::ClientClosed => {
                tracing::error!("faucet client is closed but requests are still coming in")
            },
            Self::ClientReturnChannelClosed => {
                tracing::error!("result channel from the faucet closed mid-request")
            },
            Self::MintFailed(error) => {
                tracing::error!(error = error.as_report(), "mint failed")
            },
            Self::ResponseBuilder(error) => {
                tracing::error!(error = error.as_report(), "failed to build response")
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
        let note_type = match self.is_private_note {
            true => NoteType::Private,
            false => NoteType::Public,
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
    State(state): State<impl AsRef<GetTokensState>>,
    Json(request): Json<RawMintRequest>,
) -> Result<impl IntoResponse, GetTokenError> {
    let state = state.as_ref();

    let request = request.validate(&state.asset_options).map_err(GetTokenError::InvalidRequest)?;
    let request_account = request.account_id;

    // Submit the request to the client and wait for the result.
    let (tx_result, rx_result) = oneshot::channel();
    state.request_sender.try_send((request, tx_result)).map_err(|err| match err {
        TrySendError::Full(_) => GetTokenError::ClientOverloaded,
        TrySendError::Closed(_) => GetTokenError::ClientClosed,
    })?;

    let (block_height, note) = rx_result
        .await
        .map_err(|_| GetTokenError::ClientReturnChannelClosed)?
        .map_err(GetTokenError::MintFailed)?;

    let note_id: NoteId = note.id();
    let note_details = NoteDetails::new(note.assets().clone(), note.recipient().clone());
    // SAFETY: NoteTag creation can only error for network execution mode, and we only use private or public.
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
