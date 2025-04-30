use std::{collections::HashMap, sync::Arc};

use miden_objects::{account::AccountId, block::BlockNumber, note::Note};
use static_files::Resource;
use tokio::sync::oneshot;

use crate::{config::FaucetConfig, handlers::FaucetRequest, static_resources};

// FAUCET STATE
// ================================================================================================

type RequestSender =
    tokio::sync::mpsc::Sender<(FaucetRequest, oneshot::Sender<(BlockNumber, Note)>)>;

/// Stores the client and additional information needed to handle requests.
///
/// The state is passed to every mint transaction request so the client is
/// shared between handler threads.
#[derive(Clone)]
pub struct FaucetState {
    pub account_id: AccountId,
    pub config: FaucetConfig,
    pub static_files: Arc<HashMap<&'static str, Resource>>,
    pub request_sender: RequestSender,
}

impl FaucetState {
    pub async fn new(
        // TODO: get rid of this.
        config: FaucetConfig,
        account_id: AccountId,
        request_sender: RequestSender,
    ) -> anyhow::Result<Self> {
        let static_files = Arc::new(static_resources::generate());

        Ok(FaucetState {
            account_id,
            config,
            static_files,
            request_sender,
        })
    }
}
