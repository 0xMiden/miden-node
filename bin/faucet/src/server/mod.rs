use std::{
    collections::BTreeSet,
    convert::Infallible,
    net::SocketAddr,
    sync::{Arc, atomic::AtomicUsize},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use axum::{
    Json, Router,
    extract::{FromRef, Query, State},
    response::sse::Event,
    routing::get,
};
use frontend::Metadata;
use get_tokens::{GetTokensState, RawMintRequest, get_tokens};
use http::{HeaderValue, Request, StatusCode};
use miden_node_utils::grpc::UrlExt;
use miden_objects::account::AccountId;
use miden_tx::utils::Serializable;
use pow::PoW;
use sha3::{Digest, Sha3_256};
use tokio::{net::TcpListener, sync::mpsc};
use tower::ServiceBuilder;
use tower_governor::{
    GovernorError, GovernorLayer,
    governor::GovernorConfigBuilder,
    key_extractor::{KeyExtractor, SmartIpKeyExtractor},
};
use tower_http::{
    cors::CorsLayer,
    set_header::SetResponseHeaderLayer,
    trace::{DefaultOnResponse, TraceLayer},
};
use tracing::Level;
use url::Url;

use crate::{
    COMPONENT,
    faucet::{FaucetId, MintRequest},
    server::get_tokens::InvalidRequest,
    types::AssetOptions,
};

mod challenge;
mod frontend;
mod get_tokens;
mod pow;

// FAUCET STATE
// ================================================================================================

type RequestSender = mpsc::Sender<(MintRequest, mpsc::Sender<Result<Event, Infallible>>)>;

/// Serves the faucet's website and handles token requests.
#[derive(Clone)]
pub struct Server {
    mint_state: GetTokensState,
    metadata: &'static Metadata,
    pow: PoW,
    api_keys: BTreeSet<String>,
    active_requests: Arc<AtomicUsize>,
}

impl Server {
    pub fn new(
        faucet_id: FaucetId,
        asset_options: AssetOptions,
        request_sender: RequestSender,
        pow_secret: &str,
        api_keys: BTreeSet<String>,
    ) -> Self {
        let mint_state = GetTokensState::new(request_sender, asset_options.clone());
        let metadata = Metadata {
            id: faucet_id,
            asset_amount_options: asset_options,
        };
        // SAFETY: Leaking is okay because we want it to live as long as the application.
        let metadata = Box::leak(Box::new(metadata));

        // Hash the string secret to [u8; 32] for PoW
        let mut hasher = Sha3_256::new();
        hasher.update(pow_secret.as_bytes());
        let secret_bytes: [u8; 32] = hasher.finalize().into();

        let pow = PoW::new(secret_bytes);

        Server {
            mint_state,
            metadata,
            pow,
            active_requests: Arc::new(AtomicUsize::new(0)),
            api_keys,
        }
    }

    // TODO: Cannot move the rate limiter creation to its own function because it requires
    // defining types that the `governor` crate does not export.
    #[allow(clippy::too_many_lines)]
    pub async fn serve(self, url: Url) -> anyhow::Result<()> {
        // Rate limits by IP. We do additional per account rate limiting in the get_tokens method.
        //
        // Limits chosen somewhat arbitrarily, but we have five assets required to load the webpage.
        // So allowing 8 in a burst seems okay.
        //
        // SAFETY: No non-zero elements, so we are okay.
        let ip_rate_limiter = GovernorConfigBuilder::default()
            .const_burst_size(8)
            .const_per_second(1)
            // The default extractor uses the peer address which is incorrect
            // if used behind a proxy.
            .key_extractor(SmartIpKeyExtractor)
            .finish()
            .unwrap();
        let ip_rate_limiter = Arc::new(ip_rate_limiter);

        let account_rate_limiter = GovernorConfigBuilder::default()
            .const_burst_size(1)
            .const_per_second(10)
            // The default extractor uses the peer address which is incorrect
            // if used behind a proxy.
            .key_extractor(AccountKeyExtractor)
            .finish()
            .unwrap();
        let account_rate_limiter = Arc::new(account_rate_limiter);

        // TODO: We should move the rate limiter parameters to the config file or as env vars. The
        // same goes for the other rate limiters in this function.
        let api_key_rate_limiter = GovernorConfigBuilder::default()
            .const_burst_size(3)
            .const_per_second(10)
            .key_extractor(ApiKeyExtractor)
            .finish()
            .unwrap();
        let api_key_rate_limiter = Arc::new(api_key_rate_limiter);

        // Rate limiter requires periodic state cleanup.
        tokio::spawn({
            let ip_rate_limiter = ip_rate_limiter.limiter().clone();
            let account_rate_limiter = account_rate_limiter.limiter().clone();
            let api_key_rate_limiter = api_key_rate_limiter.limiter().clone();
            async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    ip_rate_limiter.retain_recent();
                    account_rate_limiter.retain_recent();
                    api_key_rate_limiter.retain_recent();
                }
            }
        });

        let app = Router::new()
                .route("/", get(frontend::get_index_html))
                .route("/index.js", get(frontend::get_index_js))
                .route("/index.css", get(frontend::get_index_css))
                .route("/background.png", get(frontend::get_background))
                .route("/favicon.ico", get(frontend::get_favicon))
                .route("/get_metadata", get(frontend::get_metadata))
                .route("/pow", get(|State(pow): State<PoW>| async move {
                    Json(pow.build_challenge())
                }))
                // TODO: This feels rather ugly, and would be nice to move but I can't figure out the types.
                .route(
                    "/get_tokens",
                    get(get_tokens)
                        .route_layer(
                            ServiceBuilder::new()
                                .layer(
                                    // The other routes are serving static files and are therefore less interesting to log.
                                    TraceLayer::new_for_http()
                                        // Pre-register the account and amount so we can fill them in in the request.
                                        //
                                        // TODO: switch input from json to query params so we can fill in here.
                                        .make_span_with(|_request: &Request<_>| {
                                            use tracing::field::Empty;
                                            tracing::info_span!(
                                                "token_request",
                                                account = Empty,
                                                note_type = Empty,
                                                amount = Empty
                                            )
                                        })
                                        .on_response(DefaultOnResponse::new().level(Level::INFO))
                                        // Disable failure logs since we already trace errors in the method.
                                        .on_failure(())
                                ))
                                .layer(
                                    ServiceBuilder::new()
                                    .layer(
                                        GovernorLayer {
                                            config: account_rate_limiter,
                                        }
                                    )
                                    .layer(
                                        GovernorLayer {
                                            config: api_key_rate_limiter,
                                        }
                                    )
                                )
                )
                .layer(
                    ServiceBuilder::new()
                        .layer(SetResponseHeaderLayer::if_not_present(
                            http::header::CACHE_CONTROL,
                            HeaderValue::from_static("no-cache"),
                        ))
                        .layer(
                            CorsLayer::new()
                                .allow_origin(tower_http::cors::Any)
                                .allow_methods(tower_http::cors::Any)
                                .allow_headers([http::header::CONTENT_TYPE]),
                        ),
                )
                .layer(GovernorLayer {
                    config: ip_rate_limiter
                })
                .with_state(self);

        let listener = url.to_socket().with_context(|| format!("failed to parse url {url}"))?;
        let listener = TcpListener::bind(listener)
            .await
            .with_context(|| format!("failed to bind TCP listener on {url}"))?;

        tracing::info!(target: COMPONENT, address = %url, "Server started");

        // The into_make... is required by the rate limiter for some reason.
        // https://github.com/benwis/tower-governor/issues/10
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .map_err(Into::into)
    }

    /// Submits a challenge to the `PoW` instance.
    ///
    /// The challenge is validated and added to the cache.
    ///
    /// # Errors
    /// Returns an error if:
    /// * The challenge is expired.
    /// * The challenge is invalid.
    /// * The challenge was already used.
    ///
    /// # Panics
    /// Panics if the current timestamp is before the UNIX epoch.
    pub(crate) fn submit_challenge(
        &self,
        challenge: &str,
        nonce: u64,
    ) -> Result<(), InvalidRequest> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("current timestamp should be greater than unix epoch")
            .as_secs();
        self.pow.submit_challenge(timestamp, challenge, nonce)
    }
}

impl FromRef<Server> for &'static Metadata {
    fn from_ref(input: &Server) -> Self {
        input.metadata
    }
}

impl FromRef<Server> for GetTokensState {
    fn from_ref(input: &Server) -> Self {
        input.mint_state.clone()
    }
}

impl FromRef<Server> for PoW {
    fn from_ref(input: &Server) -> Self {
        // Clone is cheap: only copies a 32-byte array and increments Arc reference counters.
        input.pow.clone()
    }
}

#[derive(Clone)]
struct AccountKeyExtractor;

// Required so we can impl Hash.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RequestAccountId(AccountId);

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
struct ApiKeyExtractor;

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
