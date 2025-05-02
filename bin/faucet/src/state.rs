use axum::extract::FromRef;
use miden_objects::{block::BlockNumber, note::Note};
use tokio::sync::oneshot;

use crate::{
    client::{FaucetId, MintRequest},
    errors::MintResult,
    frontend::StaticResources,
    handlers::GetTokensState,
    types::AssetOptions,
};

// FAUCET STATE
// ================================================================================================

type RequestSender =
    tokio::sync::mpsc::Sender<(MintRequest, oneshot::Sender<MintResult<(BlockNumber, Note)>>)>;

/// The state associated with the facuet's frontend server.
///
/// Mint requests are submitted to the faucet using a channel.
#[derive(Clone)]
pub struct ServerState {
    mint_state: GetTokensState,
    static_files: &'static StaticResources,
}

impl ServerState {
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

        ServerState { mint_state, static_files }
    }
}

impl FromRef<ServerState> for &'static StaticResources {
    fn from_ref(input: &ServerState) -> Self {
        input.static_files
    }
}

impl FromRef<ServerState> for GetTokensState {
    fn from_ref(input: &ServerState) -> Self {
        input.mint_state.clone()
    }
}
