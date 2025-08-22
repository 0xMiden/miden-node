use std::collections::HashSet;
use std::convert::Infallible;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use axum::Router;
use axum::extract::FromRef;
use axum::response::sse::Event;
use axum::routing::{MethodRouter, get};
use frontend::Metadata;
use get_tokens::{GetTokensState, get_tokens};
use http::{HeaderValue, Request};
use miden_node_utils::grpc::UrlExt;
use miden_objects::account::AccountId;
use pow::PoW;
use sha3::{Digest, Sha3_256};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tower::ServiceBuilder;
use tower_http::cors::CorsLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing::Level;
use url::Url;

use crate::COMPONENT;
use crate::faucet::{FaucetId, MintRequest};
use crate::server::get_pow::get_pow;
use crate::server::get_tokens::MintRequestError;
use crate::types::AssetOptions;

mod api_key;
mod challenge;
mod frontend;
mod get_pow;
mod get_tokens;
mod pow;
pub use api_key::ApiKey;
pub use pow::PoWConfig;

// FAUCET STATE
// ================================================================================================

type RequestSender = mpsc::Sender<(MintRequest, mpsc::Sender<Result<Event, Infallible>>)>;

/// Serves the faucet's website and handles token requests.
#[derive(Clone)]
pub struct Server {
    mint_state: GetTokensState,
    metadata: &'static Metadata,
    pow: PoW,
    api_keys: HashSet<ApiKey>,
}

impl Server {
    pub fn new(
        faucet_id: FaucetId,
        asset_options: AssetOptions,
        request_sender: RequestSender,
        pow_secret: &str,
        pow_config: PoWConfig,
        api_keys: &[ApiKey],
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

        let pow = PoW::new(secret_bytes, pow_config);

        Server {
            mint_state,
            metadata,
            pow,
            api_keys: api_keys.iter().cloned().collect::<HashSet<_>>(),
        }
    }

    #[allow(clippy::too_many_lines)]
    pub async fn serve(self, url: Url) -> anyhow::Result<()> {
        let app = Router::new()
            .route("/", get(frontend::get_index_html))
            .route("/index.js", get(frontend::get_index_js))
            .route("/index.css", get(frontend::get_index_css))
            .route("/background.png", get(frontend::get_background))
            .route("/favicon.ico", get(frontend::get_favicon))
            .route("/get_metadata", get(frontend::get_metadata))
            .route("/pow", get(get_pow))
            .route("/get_tokens", Self::get_tokens_router())
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
            .with_state(self);

        let listener = url.to_socket().with_context(|| format!("failed to parse url {url}"))?;
        let listener = TcpListener::bind(listener)
            .await
            .with_context(|| format!("failed to bind TCP listener on {url}"))?;

        tracing::info!(target: COMPONENT, address = %url, "Server started");

        axum::serve(listener, app).await.map_err(Into::into)
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
        account_id: AccountId,
        api_key: &ApiKey,
    ) -> Result<(), MintRequestError> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("current timestamp should be greater than unix epoch")
            .as_secs();
        self.pow.submit_challenge(account_id, api_key, challenge, nonce, timestamp)
    }

    fn get_tokens_router() -> MethodRouter<Server> {
        get(get_tokens).route_layer(
            ServiceBuilder::new().layer(
                // The other routes are serving static files and are therefore less interesting to
                // log.
                TraceLayer::new_for_http()
                    // Pre-register the account and amount so we can fill them in the request.
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
                    .on_failure(()),
            ),
        )
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
