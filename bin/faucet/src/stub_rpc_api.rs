use anyhow::Context;
use miden_node_proto::generated::{
    block::BlockHeader,
    requests::{
        CheckNullifiersByPrefixRequest, CheckNullifiersRequest, GetAccountDetailsRequest,
        GetAccountProofsRequest, GetAccountStateDeltaRequest, GetBlockByNumberRequest,
        GetBlockHeaderByNumberRequest, GetNotesByIdRequest, SubmitProvenTransactionRequest,
        SyncNoteRequest, SyncStateRequest,
    },
    responses::{
        CheckNullifiersByPrefixResponse, CheckNullifiersResponse, GetAccountDetailsResponse,
        GetAccountProofsResponse, GetAccountStateDeltaResponse, GetBlockByNumberResponse,
        GetBlockHeaderByNumberResponse, GetNotesByIdResponse, RpcStatusResponse,
        SubmitProvenTransactionResponse, SyncNoteResponse, SyncStateResponse,
    },
    rpc::api_server,
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
        _request: Request<CheckNullifiersRequest>,
    ) -> Result<Response<CheckNullifiersResponse>, Status> {
        unimplemented!();
    }

    async fn check_nullifiers_by_prefix(
        &self,
        _request: Request<CheckNullifiersByPrefixRequest>,
    ) -> Result<Response<CheckNullifiersByPrefixResponse>, Status> {
        unimplemented!();
    }

    async fn get_block_header_by_number(
        &self,
        _request: Request<GetBlockHeaderByNumberRequest>,
    ) -> Result<Response<GetBlockHeaderByNumberResponse>, Status> {
        let mock_chain = MockChain::new();

        let block_header = BlockHeader::from(mock_chain.latest_block_header()).into();

        Ok(Response::new(GetBlockHeaderByNumberResponse {
            block_header,
            mmr_path: None,
            chain_length: None,
        }))
    }

    async fn sync_state(
        &self,
        _request: Request<SyncStateRequest>,
    ) -> Result<Response<SyncStateResponse>, Status> {
        unimplemented!();
    }

    async fn sync_notes(
        &self,
        _request: Request<SyncNoteRequest>,
    ) -> Result<Response<SyncNoteResponse>, Status> {
        unimplemented!();
    }

    async fn get_notes_by_id(
        &self,
        _request: Request<GetNotesByIdRequest>,
    ) -> Result<Response<GetNotesByIdResponse>, Status> {
        unimplemented!();
    }

    async fn submit_proven_transaction(
        &self,
        _request: Request<SubmitProvenTransactionRequest>,
    ) -> Result<Response<SubmitProvenTransactionResponse>, Status> {
        Ok(Response::new(SubmitProvenTransactionResponse { block_height: 0 }))
    }

    async fn get_account_details(
        &self,
        _request: Request<GetAccountDetailsRequest>,
    ) -> Result<Response<GetAccountDetailsResponse>, Status> {
        Err(Status::not_found("account not found"))
    }

    async fn get_block_by_number(
        &self,
        _request: Request<GetBlockByNumberRequest>,
    ) -> Result<Response<GetBlockByNumberResponse>, Status> {
        unimplemented!()
    }

    async fn get_account_state_delta(
        &self,
        _request: Request<GetAccountStateDeltaRequest>,
    ) -> Result<Response<GetAccountStateDeltaResponse>, Status> {
        unimplemented!()
    }

    async fn get_account_proofs(
        &self,
        _request: Request<GetAccountProofsRequest>,
    ) -> Result<Response<GetAccountProofsResponse>, Status> {
        unimplemented!()
    }

    async fn status(&self, _request: Request<()>) -> Result<Response<RpcStatusResponse>, Status> {
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
