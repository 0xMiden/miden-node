use axum::{
    Json,
    extract::{Query, State},
    response::IntoResponse,
};
use http::StatusCode;
use miden_node_utils::ErrorReport;
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
        .map_err(InvalidPowRequest::InvalidAccountId)?;
        let api_key = ApiKey::decode(self.api_key).map_err(|_| InvalidPowRequest::InvalidApiKey)?;
        Ok(PowRequest { account_id, api_key })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InvalidPowRequest {
    #[error("account ID failed to parse")]
    InvalidAccountId(#[source] AccountIdError),
    #[error("API key failed to parse")]
    InvalidApiKey,
}

impl IntoResponse for InvalidPowRequest {
    fn into_response(self) -> axum::response::Response {
        (StatusCode::BAD_REQUEST, self.as_report()).into_response()
    }
}
