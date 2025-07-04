use anyhow::Context;
use miden_node_proto::generated::{
    blockchain::BlockHeader,
    rpc::{RpcStatus, api_server},
    shared::{
        AccountProofs, AccountStateDelta, BlockHeaderByNumber, CheckNullifiers,
        CheckNullifiersByPrefix, CheckNullifiersByPrefixResult, CheckNullifiersResult,
        GetAccountDetails, GetAccountDetailsResult, GetAccountProofs, GetAccountStateDelta,
        GetBlockByNumber, GetBlockByNumberResult, GetBlockHeaderByNumber, GetNotesById,
        GetNotesByIdResult, ProvenTransaction, SubmitProvenTransaction, SyncNote, SyncNoteResult,
        SyncState, SyncStateResult,
    },
};
use miden_node_utils::cors::cors_for_grpc_web_layer;
use miden_testing::MockChain;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Request, Response, Status};
use tonic_web::GrpcWebLayer;
use url::Url;

#[derive(Clone)]
pub struct StubRpcApi;

#[tonic::async_trait]
impl api_server::Api for StubRpcApi {
    async fn check_nullifiers(
        &self,
        _request: Request<CheckNullifiers>,
    ) -> Result<Response<CheckNullifiersResult>, Status> {
        unimplemented!();
    }

    async fn check_nullifiers_by_prefix(
        &self,
        _request: Request<CheckNullifiersByPrefix>,
    ) -> Result<Response<CheckNullifiersByPrefixResult>, Status> {
        unimplemented!();
    }

    async fn get_block_header_by_number(
        &self,
        _request: Request<GetBlockHeaderByNumber>,
    ) -> Result<Response<BlockHeaderByNumber>, Status> {
        let mock_chain = MockChain::new();

        let block_header = BlockHeader::from(mock_chain.latest_block_header()).into();

        Ok(Response::new(BlockHeaderByNumber {
            block_header,
            mmr_path: None,
            chain_length: None,
        }))
    }

    async fn sync_state(
        &self,
        _request: Request<SyncState>,
    ) -> Result<Response<SyncStateResult>, Status> {
        unimplemented!();
    }

    async fn sync_notes(
        &self,
        _request: Request<SyncNote>,
    ) -> Result<Response<SyncNoteResult>, Status> {
        unimplemented!();
    }

    async fn get_notes_by_id(
        &self,
        _request: Request<GetNotesById>,
    ) -> Result<Response<GetNotesByIdResult>, Status> {
        unimplemented!();
    }

    async fn submit_proven_transaction(
        &self,
        _request: Request<SubmitProvenTransaction>,
    ) -> Result<Response<ProvenTransaction>, Status> {
        Ok(Response::new(ProvenTransaction { block_height: 0 }))
    }

    async fn get_account_details(
        &self,
        _request: Request<GetAccountDetails>,
    ) -> Result<Response<GetAccountDetailsResult>, Status> {
        Err(Status::not_found("account not found"))
    }

    async fn get_block_by_number(
        &self,
        _request: Request<GetBlockByNumber>,
    ) -> Result<Response<GetBlockByNumberResult>, Status> {
        unimplemented!()
    }

    async fn get_account_state_delta(
        &self,
        _request: Request<GetAccountStateDelta>,
    ) -> Result<Response<AccountStateDelta>, Status> {
        unimplemented!()
    }

    async fn get_account_proofs(
        &self,
        _request: Request<GetAccountProofs>,
    ) -> Result<Response<AccountProofs>, Status> {
        unimplemented!()
    }

    async fn status(&self, _request: Request<()>) -> Result<Response<RpcStatus>, Status> {
        unimplemented!()
    }
}

pub async fn serve_stub(endpoint: &Url) -> anyhow::Result<()> {
    let addr = endpoint
        .socket_addrs(|| None)
        .context("failed to convert endpoint to socket address")?
        .into_iter()
        .next()
        .unwrap();

    let listener = TcpListener::bind(addr).await?;
    let api_service = api_server::ApiServer::new(StubRpcApi);

    tonic::transport::Server::builder()
        .accept_http1(true)
        .layer(cors_for_grpc_web_layer())
        .layer(GrpcWebLayer::new())
        .add_service(api_service)
        .serve_with_incoming(TcpListenerStream::new(listener))
        .await
        .context("failed to serve stub RPC API")
}
