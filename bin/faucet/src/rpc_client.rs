use std::str::FromStr;

use anyhow::Context;
use miden_node_proto::generated::{
    requests::{
        GetAccountDetailsRequest, GetBlockHeaderByNumberRequest, SubmitProvenTransactionRequest,
    },
    rpc::api_client::ApiClient as GeneratedClient,
};
use miden_objects::{
    account::Account,
    block::{BlockHeader, BlockNumber},
    transaction::ProvenTransaction,
};
use miden_tx::utils::{Deserializable, Serializable};
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};
use url::Url;

use crate::faucet::FaucetId;

#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    #[error("gRPC error encountered")]
    Transport(#[source] tonic::Status),
    #[error("error parsing the gRPC response")]
    ResponseParsing(#[source] anyhow::Error),
}

pub struct RpcClient {
    inner: GeneratedClient<Channel>,
}

impl RpcClient {
    /// Creates an RPC client to the given address.
    ///
    /// Connection is lazy and will re-establish in the background on disconnection.
    pub fn connect_lazy(url: &Url) -> Result<Self, tonic::transport::Error> {
        let client = Endpoint::from_str(url.as_ref())?
            .tls_config(ClientTlsConfig::default().with_native_roots())?
            .connect_lazy();
        let client = GeneratedClient::new(client);

        Ok(Self { inner: client })
    }

    pub async fn get_genesis_header(&mut self) -> Result<BlockHeader, RpcError> {
        let request = GetBlockHeaderByNumberRequest {
            block_num: BlockNumber::GENESIS.as_u32().into(),
            include_mmr_proof: None,
        };
        let response = self
            .inner
            .get_block_header_by_number(request)
            .await
            .map_err(RpcError::Transport)?;

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
        let request = GetAccountDetailsRequest { account_id: Some(id.inner().into()) };

        let account_info = self
            .inner
            .get_account_details(request)
            .await
            .map_err(RpcError::Transport)?
            .into_inner()
            .details
            .context("details field is missing")
            .map_err(RpcError::ResponseParsing)?;

        let details = account_info
            .details
            .context("account_info.details field is empty")
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
        let request = SubmitProvenTransactionRequest { transaction: tx.to_bytes() };

        self.inner
            .submit_proven_transaction(request)
            .await
            .map(|response| response.into_inner().block_height.into())
            .map_err(RpcError::Transport)
    }
}
