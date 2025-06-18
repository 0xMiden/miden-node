use std::{
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
};
use miden_node_utils::ErrorReport;
use miden_objects::{AccountIdError, account::AccountId};
use serde::Deserialize;
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio_stream::{Stream, wrappers::ReceiverStream};
use tracing::{error, instrument};

use super::Server;
use crate::{
    COMPONENT,
    faucet::MintRequest,
    server::ApiKey,
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
    pub challenge: Option<String>,
    pub nonce: Option<u64>,
    pub api_key: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum InvalidRequest {
    #[error("account ID failed to parse")]
    AccountId(#[source] AccountIdError),
    #[error("asset amount {0} is not one of the provided options")]
    AssetAmount(u64),
    #[error("API key {0} is invalid")]
    InvalidApiKey(String),
    #[error("invalid POW solution")]
    InvalidPoW,
    #[error("POW parameters are missing")]
    MissingPowParameters,
    #[error("server signatures do not match")]
    ServerSignaturesDoNotMatch,
    #[error("server timestamp expired, received: {0}, current time: {1}")]
    ExpiredServerTimestamp(u64, u64),
    #[error("challenge already used")]
    ChallengeAlreadyUsed,
    #[error("account is rate limited")]
    RateLimited,
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

impl IntoResponse for GetTokenError {
    fn into_response(self) -> Response {
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
    ///   - the API key is invalid
    ///   - the challenge is missing or invalid
    ///   - the nonce is missing or doesn't solve the challenge
    ///   - the challenge timestamp is expired
    ///   - the challenge has already been used
    #[instrument(level = "debug", target = COMPONENT, name = "faucet.server.validate", skip_all)]
    fn validate(self, server: &Server) -> Result<MintRequest, InvalidRequest> {
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

        let asset_amount = server
            .mint_state
            .asset_options
            .validate(self.asset_amount)
            .ok_or(InvalidRequest::AssetAmount(self.asset_amount))?;

        let api_key = self.api_key.as_deref().map(ApiKey::decode).transpose()?.unwrap_or_default();

        // Update the active requests counter for the API key
        server
            .active_requests_per_key
            .lock()
            .expect("active requests per key lock should be released")
            .get(&api_key)
            .ok_or(InvalidRequest::InvalidApiKey(self.api_key.unwrap_or_default()))?
            .fetch_add(1, Ordering::Relaxed);

        // Validate Challenge and nonce
        let challenge_str = self.challenge.ok_or(InvalidRequest::MissingPowParameters)?;
        let nonce = self.nonce.ok_or(InvalidRequest::MissingPowParameters)?;

        server.submit_challenge(&challenge_str, nonce, account_id, &api_key)?;

        Ok(MintRequest { account_id, note_type, asset_amount })
    }
}

/// Guard that automatically tracks the lifecycle of an active request.
///
/// An "active request" represents any request currently being handled by the system,
/// whether it's being validated, queued, or processed.
struct ActiveRequestGuard {
    active_count: Arc<AtomicUsize>,
}

impl ActiveRequestGuard {
    fn new(active_count: Arc<AtomicUsize>) -> Self {
        active_count.fetch_add(1, Ordering::Relaxed);
        Self { active_count }
    }
}

impl Drop for ActiveRequestGuard {
    fn drop(&mut self) {
        self.active_count.fetch_sub(1, Ordering::Relaxed);
    }
}

#[instrument(
    parent = None, target = COMPONENT, name = "faucet.server.get_tokens", skip_all,
    fields(
        account_id = %request.account_id,
        is_private_note = %request.is_private_note,
        asset_amount = %request.asset_amount,
    )
)]
pub async fn get_tokens(
    State(server): State<Server>,
    Query(request): Query<RawMintRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, GetTokenError> {
    // If the API key is invalid, return an error immediately. Any other errors are sent to the
    // client via SSE.
    let api_key = request
        .api_key
        .as_deref()
        .map(ApiKey::decode)
        .transpose()
        .map_err(GetTokenError::InvalidRequest)?
        .unwrap_or_default();
    let key_active_requests = server
        .active_requests_per_key
        .lock()
        .expect("active requests per key lock should be released")
        .get(&api_key)
        .ok_or(GetTokenError::InvalidRequest(InvalidRequest::InvalidApiKey(
            request.api_key.clone().unwrap_or_default(),
        )))?
        .clone();

    // Track this as an active request for the entire duration
    let _active_guard = ActiveRequestGuard::new(key_active_requests.clone());

    let current_active_requests = key_active_requests.load(Ordering::Relaxed);
    server.pow.adjust_difficulty(current_active_requests, api_key);

    // Response channel with buffer size 5 since there are currently 5 possible updates
    let (tx_result_notifier, rx_result) = mpsc::channel(5);

    let mint_error = request
        .validate(&server)
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
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    #[test]
    fn test_active_request_guard_increments_on_creation() {
        let active_count = Arc::new(AtomicUsize::new(0));

        assert_eq!(active_count.load(Ordering::Relaxed), 0);

        let _guard = ActiveRequestGuard::new(active_count.clone());
        assert_eq!(active_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_active_request_guard_decrements_on_drop() {
        let active_count = Arc::new(AtomicUsize::new(0));

        {
            let _guard = ActiveRequestGuard::new(active_count.clone());
            assert_eq!(active_count.load(Ordering::Relaxed), 1);
        } // Guard dropped here

        assert_eq!(active_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_multiple_active_request_guards() {
        let active_count = Arc::new(AtomicUsize::new(0));

        let guard1 = ActiveRequestGuard::new(active_count.clone());
        assert_eq!(active_count.load(Ordering::Relaxed), 1);

        let guard2 = ActiveRequestGuard::new(active_count.clone());
        assert_eq!(active_count.load(Ordering::Relaxed), 2);

        let guard3 = ActiveRequestGuard::new(active_count.clone());
        assert_eq!(active_count.load(Ordering::Relaxed), 3);

        drop(guard2);
        assert_eq!(active_count.load(Ordering::Relaxed), 2);

        drop(guard1);
        assert_eq!(active_count.load(Ordering::Relaxed), 1);

        drop(guard3);
        assert_eq!(active_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_active_request_guard_behavior_on_error_scenarios() {
        let active_count = Arc::new(AtomicUsize::new(0));

        // Simulate validation error - active guard created
        {
            let _active_guard = ActiveRequestGuard::new(active_count.clone());
            assert_eq!(active_count.load(Ordering::Relaxed), 1);

            // Validation fails, request doesn't proceed
            // Guard will be dropped when going out of scope
        }

        assert_eq!(active_count.load(Ordering::Relaxed), 0);

        // Simulate queue full error - active guard created
        {
            let _active_guard = ActiveRequestGuard::new(active_count.clone());
            assert_eq!(active_count.load(Ordering::Relaxed), 1);

            // Queue is full, request doesn't proceed
            // Guard will be dropped when going out of scope
        }

        assert_eq!(active_count.load(Ordering::Relaxed), 0);
    }
}
