use axum::{
    Json,
    extract::{Query, State},
    response::IntoResponse,
};
use http::StatusCode;
use miden_objects::{AccountIdError, account::AccountId};
use serde::Deserialize;

use crate::server::{ApiKey, pow::PoW};

// ENDPOINT
// ================================================================================================

pub async fn get_pow(
    State(pow): State<PoW>,
    Query(params): Query<RawPowRequest>,
) -> Result<impl IntoResponse, InvalidPowRequest> {
    let request = params.validate()?;
    let challenge = pow.build_challenge(request);
    Ok(Json(challenge))
}

// REQUEST VALIDATION
// ================================================================================================

/// Used to receive the initial `get_pow` request from the user.
#[derive(Deserialize)]
pub struct RawPowRequest {
    pub account_id: String,
    pub api_key: Option<String>,
}

/// Validated and parsed `RawPowRequest`.
pub struct PowRequest {
    pub account_id: AccountId,
    pub api_key: ApiKey,
}

impl RawPowRequest {
    pub fn validate(self) -> Result<PowRequest, InvalidPowRequest> {
        let account_id = if self.account_id.starts_with("0x") {
            AccountId::from_hex(&self.account_id)
        } else {
            AccountId::from_bech32(&self.account_id).map(|(_, account_id)| account_id)
        }
        .map_err(InvalidPowRequest::InvalidAccount)?;

        let api_key = self
            .api_key
            .as_deref()
            .map(ApiKey::decode)
            .transpose()
            .map_err(|_| InvalidPowRequest::InvalidApiKey)?
            .unwrap_or_default();

        Ok(PowRequest { account_id, api_key })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InvalidPowRequest {
    #[error("account address failed to parse")]
    InvalidAccount(#[source] AccountIdError),
    #[error("API key failed to parse")]
    InvalidApiKey,
}

impl InvalidPowRequest {
    /// Take care to not expose internal errors here.
    fn user_facing_error(&self) -> String {
        match self {
            Self::InvalidAccount(_) => "Invalid Account address".to_owned(),
            Self::InvalidApiKey => "Invalid API key".to_owned(),
        }
    }
}

impl IntoResponse for InvalidPowRequest {
    fn into_response(self) -> axum::response::Response {
        (StatusCode::BAD_REQUEST, self.user_facing_error()).into_response()
    }
}
