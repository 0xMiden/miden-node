use anyhow::Context;
use miden_node_proto::generated::rpc::api_server;
use miden_node_proto::generated::{self as proto};
use miden_node_utils::cors::cors_for_grpc_web_layer;
use miden_node_utils::panic::{CatchPanicLayer, catch_panic_layer_fn};
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
        _request: Request<proto::rpc_store::NullifierList>,
    ) -> Result<Response<proto::rpc_store::CheckNullifiersResponse>, Status> {
        unimplemented!();
    }

    async fn check_nullifiers_by_prefix(
        &self,
        _request: Request<proto::rpc_store::CheckNullifiersByPrefixRequest>,
    ) -> Result<Response<proto::rpc_store::CheckNullifiersByPrefixResponse>, Status> {
        unimplemented!();
    }

    async fn get_block_header_by_number(
        &self,
        _request: Request<proto::shared::BlockHeaderByNumberRequest>,
    ) -> Result<Response<proto::shared::BlockHeaderByNumberResponse>, Status> {
        let mock_chain = MockChain::new();

        let block_header =
            proto::blockchain::BlockHeader::from(mock_chain.latest_block_header()).into();

        Ok(Response::new(proto::shared::BlockHeaderByNumberResponse {
            block_header,
            mmr_path: None,
            chain_length: None,
        }))
    }

    async fn sync_state(
        &self,
        _request: Request<proto::rpc_store::SyncStateRequest>,
    ) -> Result<Response<proto::rpc_store::SyncStateResponse>, Status> {
        unimplemented!();
    }

    async fn sync_notes(
        &self,
        _request: Request<proto::rpc_store::SyncNotesRequest>,
    ) -> Result<Response<proto::rpc_store::SyncNotesResponse>, Status> {
        unimplemented!();
    }

    async fn get_notes_by_id(
        &self,
        _request: Request<proto::note::NoteIdList>,
    ) -> Result<Response<proto::note::CommittedNoteList>, Status> {
        unimplemented!();
    }

    async fn submit_proven_transaction(
        &self,
        _request: Request<proto::transaction::ProvenTransaction>,
    ) -> Result<Response<proto::block_producer::SubmitProvenTransactionResponse>, Status> {
        Ok(Response::new(proto::block_producer::SubmitProvenTransactionResponse {
            block_height: 0,
        }))
    }

    async fn submit_proven_batch(
        &self,
        _request: tonic::Request<proto::transaction::ProvenTransactionBatch>,
    ) -> Result<tonic::Response<proto::block_producer::SubmitProvenBatchResponse>, Status> {
        Ok(Response::new(proto::block_producer::SubmitProvenBatchResponse {
            block_height: 0,
        }))
    }

    async fn get_account_details(
        &self,
        _request: Request<proto::account::AccountId>,
    ) -> Result<Response<proto::account::AccountDetails>, Status> {
        Err(Status::not_found("account not found"))
    }

    async fn get_block_by_number(
        &self,
        _request: Request<proto::blockchain::BlockNumber>,
    ) -> Result<Response<proto::blockchain::MaybeBlock>, Status> {
        unimplemented!()
    }

    async fn get_account_proofs(
        &self,
        _request: Request<proto::rpc_store::AccountProofsRequest>,
    ) -> Result<Response<proto::rpc_store::AccountProofs>, Status> {
        unimplemented!()
    }

    async fn status(
        &self,
        _request: Request<()>,
    ) -> Result<Response<proto::rpc::RpcStatus>, Status> {
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
        .layer(CatchPanicLayer::custom(catch_panic_layer_fn))
        .layer(cors_for_grpc_web_layer())
        .layer(GrpcWebLayer::new())
        .add_service(api_service)
        .serve_with_incoming(TcpListenerStream::new(listener))
        .await
        .context("failed to serve stub RPC API")
}
