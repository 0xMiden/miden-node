use anyhow::Context;
use miden_node_proto::generated::{
    account as account_proto, block_producer as block_producer_proto,
    blockchain as blockchain_proto,
    blockchain::BlockHeader,
    note as note_proto,
    rpc::{RpcStatus, api_server},
    store as store_proto, transaction as transaction_proto,
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
        _request: Request<store_proto::Nullifiers>,
    ) -> Result<Response<store_proto::CheckNullifiersResponse>, Status> {
        unimplemented!();
    }

    async fn check_nullifiers_by_prefix(
        &self,
        _request: Request<store_proto::CheckNullifiersByPrefixRequest>,
    ) -> Result<Response<store_proto::CheckNullifiersByPrefixResponse>, Status> {
        unimplemented!();
    }

    async fn get_block_header_by_number(
        &self,
        _request: Request<store_proto::BlockHeaderByNumberRequest>,
    ) -> Result<Response<store_proto::BlockHeaderByNumberResponse>, Status> {
        let mock_chain = MockChain::new();

        let block_header = BlockHeader::from(mock_chain.latest_block_header()).into();

        Ok(Response::new(store_proto::BlockHeaderByNumberResponse {
            block_header,
            mmr_path: None,
            chain_length: None,
        }))
    }

    async fn sync_state(
        &self,
        _request: Request<store_proto::SyncStateRequest>,
    ) -> Result<Response<store_proto::SyncStateResponse>, Status> {
        unimplemented!();
    }

    async fn sync_notes(
        &self,
        _request: Request<store_proto::SyncNotesRequest>,
    ) -> Result<Response<store_proto::SyncNotesResponse>, Status> {
        unimplemented!();
    }

    async fn get_notes_by_id(
        &self,
        _request: Request<note_proto::NoteIdList>,
    ) -> Result<Response<note_proto::CommittedNoteList>, Status> {
        unimplemented!();
    }

    async fn submit_proven_transaction(
        &self,
        _request: Request<transaction_proto::ProvenTransaction>,
    ) -> Result<Response<block_producer_proto::SubmitProvenTransactionResponse>, Status> {
        Ok(Response::new(block_producer_proto::SubmitProvenTransactionResponse {
            block_height: 0,
        }))
    }

    async fn get_account_details(
        &self,
        _request: Request<account_proto::AccountId>,
    ) -> Result<Response<account_proto::AccountDetails>, Status> {
        Err(Status::not_found("account not found"))
    }

    async fn get_block_by_number(
        &self,
        _request: Request<blockchain_proto::BlockNumber>,
    ) -> Result<Response<blockchain_proto::MaybeBlock>, Status> {
        unimplemented!()
    }

    async fn get_account_state_delta(
        &self,
        _request: Request<store_proto::GetAccountStateDeltaRequest>,
    ) -> Result<Response<store_proto::AccountStateDelta>, Status> {
        unimplemented!()
    }

    async fn get_account_proofs(
        &self,
        _request: Request<store_proto::GetAccountProofsRequest>,
    ) -> Result<Response<store_proto::AccountProofs>, Status> {
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
