use std::{net::SocketAddr, time::Duration};

use anyhow::Context;
use miden_node_proto::generated::ntx_builder::api_server;
use server::NtxBuilderApi;
use store::StoreClient;
use tokio::{net::TcpListener, time};
use tokio_stream::wrappers::TcpListenerStream;
use tower_http::trace::TraceLayer;
use tracing::{debug, info, warn};
use url::Url;

use crate::COMPONENT;

mod server;

/// Interval for checking pending notes
const NOTE_CHECK_INTERVAL_MS: u64 = 100;

//mod data_store;
mod store;

/// Network Transaction Builder
pub struct NetworkTransactionBuilder {
    /// The address of the network transaction builder gRPC server.
    pub address: SocketAddr,
    /// URL of the store gRPC server.
    pub store_url: Url,
}

impl NetworkTransactionBuilder {
    /// Serves the network transaction builder RPC API
    ///
    /// Note: Executes in place (i.e. not spawned) and will run indefinitely until
    ///       a fatal error is encountered.
    pub async fn serve(self) -> anyhow::Result<()> {
        info!(
            target: COMPONENT,
            endpoint=?self.address,
            "Starting network transaction builder server"
        );

        let store = StoreClient::new(&self.store_url);
        let unconsumed_network_notes = store.get_unconsumed_network_notes().await?;

        let api = NtxBuilderApi::new(unconsumed_network_notes);
        let api_state = api.state();
        let api_service = api_server::ApiServer::new(api);

        let listener = TcpListener::bind(self.address).await?;

        // Spawn the ticker task to process pending notes periodically
        let tick_handle = tokio::spawn(async move {
            let mut interval = time::interval(Duration::from_millis(NOTE_CHECK_INTERVAL_MS));

            loop {
                interval.tick().await;

                {
                    let mut state_guard = match api_state.lock() {
                        Ok(guard) => guard,
                        Err(e) => {
                            warn!(target: COMPONENT, error=%e, "Failed to lock API state - poisoned lock");
                            continue;
                        },
                    };

                    // Get a tag with pending notes
                    if let Some((tag, _notes)) = state_guard.take_next_notes_by_tag() {
                        debug!(
                            target: COMPONENT,
                            tag=%tag,
                            "Found note tag to process"
                        );
                        // TODO: call executor
                    }
                }
            }
        });

        // Start the RPC server
        let server_result = tonic::transport::Server::builder()
            .accept_http1(true)
            .layer(TraceLayer::new_for_grpc())
            .add_service(api_service)
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await;

        // If the server stops, we should also stop the ticker
        tick_handle.abort();

        // Return the server result
        server_result.context("failed to serve network transaction builder API")
    }
}
