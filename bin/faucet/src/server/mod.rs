use anyhow::Context;
use axum::{
    Router,
    extract::FromRef,
    routing::{get, post},
};
use frontend::StaticResources;
use get_tokens::{GetTokensState, get_tokens};
use http::{HeaderValue, Request};
use miden_objects::{block::BlockNumber, note::Note};
use tokio::{net::TcpListener, sync::oneshot};
use tower::ServiceBuilder;
use tower_http::{
    cors::CorsLayer,
    set_header::SetResponseHeaderLayer,
    trace::{DefaultOnResponse, TraceLayer},
};
use tracing::Level;

use crate::{
    COMPONENT,
    faucet::{FaucetId, MintRequest},
    types::AssetOptions,
};

mod frontend;
mod get_tokens;

// FAUCET STATE
// ================================================================================================

type RequestSender = tokio::sync::mpsc::Sender<(MintRequest, oneshot::Sender<(BlockNumber, Note)>)>;

/// Serves the faucet's website and handles token requests.
#[derive(Clone)]
pub struct Server {
    mint_state: GetTokensState,
    static_files: &'static StaticResources,
}

impl Server {
    pub fn new(
        faucet_id: FaucetId,
        asset_options: AssetOptions,
        request_sender: RequestSender,
    ) -> Self {
        let mint_state = GetTokensState {
            request_sender,
            asset_options: asset_options.clone(),
        };
        let static_files = StaticResources::new(faucet_id, asset_options).leak();

        Server { mint_state, static_files }
    }

    pub async fn serve(self, port: u16) -> anyhow::Result<()> {
        let app = Router::new()
                .route("/", get(frontend::get_index_html))
                .route("/index.js", get(frontend::get_index_js))
                .route("/index.css", get(frontend::get_index_css))
                .route("/background.png", get(frontend::get_background))
                .route("/favicon.ico", get(frontend::get_favicon))
                .route("/get_metadata", get(frontend::get_metadata))
                // TODO: This feels rather ugly, and would be nice to move but I can't figure out the types.
                .route(
                    "/get_tokens",
                    post(get_tokens).layer(
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
                    ),
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
                .with_state(self);

        let server_addr = format!("0.0.0.0:{port}");
        let listener = TcpListener::bind(&server_addr)
            .await
            .with_context(|| format!("failed to bind TCP listener on {server_addr}"))?;

        tracing::info!(target: COMPONENT, address = %server_addr, "Server started");

        axum::serve(listener, app).await.map_err(Into::into)
    }
}

impl FromRef<Server> for &'static StaticResources {
    fn from_ref(input: &Server) -> Self {
        input.static_files
    }
}

impl FromRef<Server> for GetTokensState {
    fn from_ref(input: &Server) -> Self {
        input.mint_state.clone()
    }
}
