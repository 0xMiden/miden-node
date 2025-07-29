use std::time::Duration;

use anyhow::Context;
use miden_node_proto::{
    clients::{Builder, Rpc as RpcClientMarker, RpcClient as InnerRpcClient},
    generated as proto,
};
use miden_objects::{
    account::Account,
    block::{BlockHeader, BlockNumber},
    transaction::ProvenTransaction,
};
use miden_tx::utils::{Deserializable, Serializable};
use url::Url;

use crate::faucet::FaucetId;

#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    #[error("gRPC error encountered")]
    Transport(#[source] Box<tonic::Status>),
    #[error("error parsing the gRPC response")]
    ResponseParsing(#[source] anyhow::Error),
}

// CLIENT
// ================================================================================================

/// Client used to interact with an RPC endpoint on a Miden node.
pub struct RpcClient {
    inner: InnerRpcClient,
}

impl RpcClient {
    /// Creates an RPC client to the given address.
    ///
    /// The connection is lazy and will re-establish in the background on disconnection.
    pub fn connect_lazy(url: &Url, timeout_ms: u64) -> Result<Self, anyhow::Error> {
        let client = Builder::new()
            .with_address(url.to_string())
            .with_tls()
            .with_timeout(Duration::from_millis(timeout_ms))
            .connect_lazy::<RpcClientMarker>()?;

        Ok(Self { inner: client })
    }

    pub async fn get_genesis_header(&mut self) -> Result<BlockHeader, RpcError> {
        let request = proto::shared::BlockHeaderByNumberRequest {
            block_num: BlockNumber::GENESIS.as_u32().into(),
            include_mmr_proof: None,
        };
        let response = self
            .inner
            .get_block_header_by_number(request)
            .await
            .map_err(|e| RpcError::Transport(e.into()))?;

        let root_block_header = response
            .into_inner()
            .block_header
            .context("block_header field is missing")
            .map_err(RpcError::ResponseParsing)?;

        root_block_header
            .try_into()
            .context("failed to parse block header")
            .map_err(RpcError::ResponseParsing)
    }

    /// Gets the latest committed faucet account state from the node.
    ///
    /// Note that this _does not_ include any uncommitted state in the mempool.
    pub async fn get_faucet_account(&mut self, id: FaucetId) -> Result<Account, RpcError> {
        let request = proto::account::AccountId { id: id.account_id.to_bytes().clone() };

        let details = self
            .inner
            .get_account_details(request)
            .await
            .map_err(|e| RpcError::Transport(e.into()))?
            .into_inner()
            .details
            .context("details field is missing")
            .map_err(RpcError::ResponseParsing)?;

        Account::read_from_bytes(&details)
            .context("failed to deserialize faucet account")
            .map_err(RpcError::ResponseParsing)
    }

    /// Submits the transaction to the node and returns the node's current block height.
    pub async fn submit_transaction(
        &mut self,
        tx: ProvenTransaction,
    ) -> Result<BlockNumber, RpcError> {
        let request = proto::transaction::ProvenTransaction { transaction: tx.to_bytes() };

        self.inner
            .submit_proven_transaction(request)
            .await
            .map(|response| response.into_inner().block_height.into())
            .map_err(|e| RpcError::Transport(e.into()))
    }
}
