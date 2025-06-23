use axum::extract::Query;
use clap::Args;
use http::{Request, StatusCode};
use miden_lib::utils::Serializable;
use miden_objects::account::AccountId;
use serde::{Deserialize, Serialize};
use tower_governor::{GovernorError, key_extractor::KeyExtractor};

use crate::server::get_tokens::RawMintRequest;

const ENV_IP_BURST_SIZE: &str = "MIDEN_FAUCET_RATE_LIMITER_IP_BURST_SIZE";
const ENV_IP_PER_SECOND: &str = "MIDEN_FAUCET_RATE_LIMITER_IP_PER_SECOND";
const ENV_ACCOUNT_BURST_SIZE: &str = "MIDEN_FAUCET_RATE_LIMITER_ACCOUNT_BURST_SIZE";
const ENV_ACCOUNT_PER_SECOND: &str = "MIDEN_FAUCET_RATE_LIMITER_ACCOUNT_PER_SECOND";
const ENV_API_KEY_BURST_SIZE: &str = "MIDEN_FAUCET_RATE_LIMITER_API_KEY_BURST_SIZE";
const ENV_API_KEY_PER_SECOND: &str = "MIDEN_FAUCET_RATE_LIMITER_API_KEY_PER_SECOND";

/// Configuration for rate limiting requests to the faucet.
///
/// We have three rate limiters:
///
/// 1. IP-based rate limiting: restricts requests from the same IP address
/// 2. Account-based rate limiting: restricts requests to `get_tokens` with the same account
/// 3. API key-based rate limiting: restricts requests using the same API key
///
/// Each rate limiter uses a token bucket algorithm with two parameters:
/// - `burst_size`: Maximum number of requests allowed in a burst before replenishment
/// - `per_second`: Rate at which tokens are replenished (1 token per X seconds)
///
/// # Examples
///
/// Given:
/// - `burst_size` = 8
/// - `per_second` = 1
///
/// This means that initially this rate limiter will allow 8 requests. Then, each second it will
/// replenish 1 token. This can be thought as a bucket that holds a maximum of 8 tokens, and
/// refills at a rate of 1 token per second. When empty, requests are blocked.
#[derive(Clone, Debug, Serialize, Deserialize, Args, Default)]
#[serde(deny_unknown_fields)]
pub struct RateLimiterConfig {
    /// Maximum number of requests allowed in a burst for IP-based rate limiting.
    #[arg(long = "rate-limiter.ip-burst-size", value_name = "U32", default_value_t = 8, env = ENV_IP_BURST_SIZE)]
    pub ip_burst_size: u32,

    /// Rate at which tokens are replenished for IP-based rate limiting (1 token per X seconds).
    #[arg(long = "rate-limiter.ip-per-second", value_name = "U64", default_value_t = 1, env = ENV_IP_PER_SECOND)]
    pub ip_per_second: u64,

    /// Maximum number of requests allowed in a burst for account-based rate limiting.
    #[arg(long = "rate-limiter.account-burst-size", value_name = "U32", default_value_t = 1, env = ENV_ACCOUNT_BURST_SIZE)]
    pub account_burst_size: u32,

    /// Rate at which tokens are replenished for account-based rate limiting (1 token per X
    /// seconds).
    #[arg(long = "rate-limiter.account-per-second", value_name = "U64", default_value_t = 10, env = ENV_ACCOUNT_PER_SECOND)]
    pub account_per_second: u64,

    /// Maximum number of requests allowed in a burst for API key-based rate limiting.
    #[arg(long = "rate-limiter.api-key-burst-size", value_name = "U32", default_value_t = 3, env = ENV_API_KEY_BURST_SIZE)]
    pub api_key_burst_size: u32,

    /// Rate at which tokens are replenished for API key-based rate limiting (1 token per X
    /// seconds).
    #[arg(long = "rate-limiter.api-key-per-second", value_name = "U64", default_value_t = 10, env = ENV_API_KEY_PER_SECOND)]
    pub api_key_per_second: u64,
}

#[derive(Clone)]
pub struct AccountKeyExtractor;

// Required so we can impl Hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestAccountId(AccountId);

impl std::hash::Hash for RequestAccountId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.to_bytes().hash(state);
    }
}

impl KeyExtractor for AccountKeyExtractor {
    type Key = RequestAccountId;

    fn extract<T>(&self, req: &Request<T>) -> Result<Self::Key, GovernorError> {
        let params = Query::<RawMintRequest>::try_from_uri(req.uri())
            .map_err(|_| GovernorError::UnableToExtractKey)?;

        if params.account_id.starts_with("0x") {
            AccountId::from_hex(&params.account_id)
        } else {
            AccountId::from_bech32(&params.account_id).map(|(_, account_id)| account_id)
        }
        .map(RequestAccountId)
        .map_err(|_| GovernorError::Other {
            code: StatusCode::BAD_REQUEST,
            msg: Some("Invalid account id".to_string()),
            headers: None,
        })
    }
}

#[derive(Clone)]
pub struct ApiKeyExtractor;

impl KeyExtractor for ApiKeyExtractor {
    type Key = String;

    fn extract<T>(&self, req: &Request<T>) -> Result<Self::Key, GovernorError> {
        let params = Query::<RawMintRequest>::try_from_uri(req.uri())
            .map_err(|_| GovernorError::UnableToExtractKey)?;

        if let Some(api_key) = params.api_key.as_ref() {
            // Requests with the same api key are rate limited together.
            Ok(api_key.to_string())
        } else {
            // The naive approach would be to use an empty string as the extracted key for this case
            // but by doing this then all non-api key requests will be grouped together and the
            // limit will be reached almost instantly. To avoid this, we want to return a "unique"
            // extracted key for each request. By concatenating the account id and the challenge
            // we get a somewhat unique key each time so this rate limiter won't affect
            // requests without an api key.
            Ok(params.account_id.clone()
                + &params.challenge.as_ref().ok_or(GovernorError::UnableToExtractKey)?.to_string())
        }
    }
}
