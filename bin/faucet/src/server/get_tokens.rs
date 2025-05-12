use std::convert::Infallible;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{
        Sse,
        sse::{Event, KeepAlive},
    },
};
use miden_node_utils::ErrorReport;
use miden_objects::{AccountIdError, account::AccountId};
use serde::Deserialize;
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio_stream::{Stream, wrappers::ReceiverStream};
use tracing::error;

use super::{
    Server,
    pow::{check_pow_solution, check_server_signature, check_server_timestamp},
};
use crate::{
    faucet::MintRequest,
    types::{AssetOptions, NoteType},
};

type RequestSender = mpsc::Sender<(MintRequest, mpsc::Sender<Result<Event, Infallible>>)>;

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
    pub pow_seed: String,
    pub pow_solution: u64,
    pub server_signature: String,
    pub server_timestamp: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum InvalidRequest {
    #[error("account ID failed to parse")]
    AccountId(#[source] AccountIdError),
    #[error("asset amount {0} is not one of the provided options")]
    AssetAmount(u64),
    #[error("invalid POW solution")]
    InvalidPoW,
    #[error("server signatures do not match")]
    ServerSignaturesDoNotMatch,
    #[error("server timestamp expired, received: {0}, current time: {1}")]
    ExpiredServerTimestamp(u64, u64),
}

pub enum GetTokenError {
    InvalidRequest(InvalidRequest),
    FaucetOverloaded,
    FaucetClosed,
}

impl GetTokenError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            Self::FaucetOverloaded | Self::FaucetClosed => StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    /// Take care to not expose internal errors here.
    fn user_facing_error(&self) -> String {
        match self {
            Self::InvalidRequest(invalid_request) => invalid_request.as_report(),
            Self::FaucetOverloaded => {
                "The faucet is currently overloaded, please try again later.".to_owned()
            },
            Self::FaucetClosed => {
                "The faucet is currently unavailable, please try again later.".to_owned()
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
        }
    }

    /// Convert the error into an SSE event and trigger a trace log.
    fn into_event(self) -> Event {
        // TODO: This is a hacky way of doing error logging, but
        // its one of the last times we have the error before
        // it becomes opaque. Should replace this by something
        // better
        self.trace();
        Event::default().event("get-tokens-error").data(
            serde_json::json!({
                "message": self.user_facing_error(),
                "status": self.status_code().to_string(),
            })
            .to_string(),
        )
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
    fn validate(
        self,
        options: &AssetOptions,
        pow_salt: &str,
    ) -> Result<MintRequest, InvalidRequest> {
        let note_type = if self.is_private_note {
            NoteType::Private
        } else {
            NoteType::Public
        };

        let account_id = if self.account_id.starts_with("0x") {
            AccountId::from_hex(&self.account_id)
        } else {
            AccountId::from_bech32(&self.account_id).map(|(_, account_id)| account_id)
        }
        .map_err(InvalidRequest::AccountId)?;

        let asset_amount = options
            .validate(self.asset_amount)
            .ok_or(InvalidRequest::AssetAmount(self.asset_amount))?;

        // Check the server timestamp
        check_server_timestamp(self.server_timestamp)?;

        // Check the server signature
        check_server_signature(
            pow_salt,
            &self.server_signature,
            &self.pow_seed,
            self.server_timestamp,
        )?;

        // Check the PoW solution
        check_pow_solution(&self.pow_seed, self.pow_solution)?;

        Ok(MintRequest { account_id, note_type, asset_amount })
    }
}

pub async fn get_tokens(
    State(server): State<Server>,
    Query(request): Query<RawMintRequest>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    // Response channel with buffer size 5 since there are currently 5 possible updates
    let (tx_result_notifier, rx_result) = mpsc::channel(5);

    let mint_error = request
        .validate(&server.mint_state.asset_options, &server.pow_salt)
        .map_err(GetTokenError::InvalidRequest)
        .and_then(|request| {
            let span = tracing::Span::current();
            span.record("account", request.account_id.to_hex());
            span.record("amount", request.asset_amount.inner());
            span.record("note_type", request.note_type.to_string());

            server
                .mint_state
                .request_sender
                .try_send((request, tx_result_notifier.clone()))
                .map_err(|err| match err {
                    TrySendError::Full(_) => GetTokenError::FaucetOverloaded,
                    TrySendError::Closed(_) => GetTokenError::FaucetClosed,
                })
        })
        .err();

    if let Some(error) = mint_error {
        tx_result_notifier.send(Ok(error.into_event())).await.unwrap();
    }

    let stream = ReceiverStream::new(rx_result);
    Sse::new(stream).keep_alive(KeepAlive::default())
}
