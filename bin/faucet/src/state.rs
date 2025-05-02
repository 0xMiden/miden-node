use axum::extract::FromRef;
use miden_objects::{block::BlockNumber, note::Note};
use tokio::sync::oneshot;

use crate::{
    client::{FaucetId, MintRequest},
    config::FaucetConfig,
    frontend::StaticResources,
    types::AssetOptions,
};

// FAUCET STATE
// ================================================================================================

type RequestSender = tokio::sync::mpsc::Sender<(MintRequest, oneshot::Sender<(BlockNumber, Note)>)>;

/// The state associated with the facuet's frontend server.
///
/// Mint requests are submitted to the faucet using a channel.
#[derive(Clone)]
pub struct ServerState {
    config: FaucetConfig,
    pub request_sender: RequestSender,
    static_files: &'static StaticResources,
}

impl ServerState {
    pub fn new(
        // TODO: get rid of this.
        config: FaucetConfig,
        faucet_id: FaucetId,
        asset_options: AssetOptions,
        request_sender: RequestSender,
    ) -> Self {
        let static_files = StaticResources::new(faucet_id, asset_options).leak();

        ServerState { config, static_files, request_sender }
    }
}

impl FromRef<ServerState> for &'static StaticResources {
    fn from_ref(input: &ServerState) -> Self {
        input.static_files
    }
}
