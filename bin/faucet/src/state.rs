use axum::extract::FromRef;
use miden_objects::{account::AccountId, block::BlockNumber, note::Note};
use tokio::sync::oneshot;

use crate::{client::MintRequest, config::FaucetConfig, frontend::StaticFiles};

// FAUCET STATE
// ================================================================================================

type RequestSender = tokio::sync::mpsc::Sender<(MintRequest, oneshot::Sender<(BlockNumber, Note)>)>;

/// The state associated with the facuet's frontend server.
///
/// Mint requests are submitted to the faucet using a channel.
#[derive(Clone)]
pub struct ServerState {
    pub account_id: AccountId,
    config: FaucetConfig,
    pub request_sender: RequestSender,
    static_files: &'static StaticFiles,
}

impl ServerState {
    pub fn new(
        // TODO: get rid of this.
        config: FaucetConfig,
        account_id: AccountId,
        request_sender: RequestSender,
    ) -> Self {
        let static_files = StaticFiles::new().leak();

        ServerState {
            account_id,
            config,
            static_files,
            request_sender,
        }
    }
}

impl FromRef<ServerState> for &'static StaticFiles {
    fn from_ref(input: &ServerState) -> Self {
        input.static_files
    }
}
