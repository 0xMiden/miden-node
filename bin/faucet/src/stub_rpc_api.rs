use miden_node_proto::generated::{
    block::BlockHeader,
    digest::Digest,
    requests::{
        CheckNullifiersByPrefixRequest, CheckNullifiersRequest, GetAccountDetailsRequest,
        GetAccountProofsRequest, GetAccountStateDeltaRequest, GetBlockByNumberRequest,
        GetBlockHeaderByNumberRequest, GetNotesByIdRequest, SubmitProvenTransactionRequest,
        SyncNoteRequest, SyncStateRequest,
    },
    responses::{
        CheckNullifiersByPrefixResponse, CheckNullifiersResponse, GetAccountDetailsResponse,
        GetAccountProofsResponse, GetAccountStateDeltaResponse, GetBlockByNumberResponse,
        GetBlockHeaderByNumberResponse, GetNotesByIdResponse, SubmitProvenTransactionResponse,
        SyncNoteResponse, SyncStateResponse,
    },
    rpc::api_server,
};
use miden_node_utils::errors::ApiError;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Request, Response, Status};
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
        // Values are taken from the default genesis block as at v0.8
        Ok(Response::new(GetBlockHeaderByNumberResponse {
            block_header: Some(BlockHeader {
                version: 1,
                prev_block_commitment: Some(Digest { d0: 0, d1: 0, d2: 0, d3: 0 }),
                block_num: 0,
                chain_commitment: Some(Digest {
                    d0: 10892410042676993129,
                    d1: 465072181589837593,
                    d2: 8905599737602832342,
                    d3: 16439138630577134987,
                }),
                account_root: Some(Digest {
                    d0: 9472777083497015232,
                    d1: 11919252219186983084,
                    d2: 8352601270190635803,
                    d3: 14509541871745412808,
                }),
                nullifier_root: Some(Digest {
                    d0: 15321474589252129342,
                    d1: 17373224439259377994,
                    d2: 15071539326562317628,
                    d3: 3312677166725950353,
                }),
                note_root: Some(Digest {
                    d0: 10650694022550988030,
                    d1: 5634734408638476525,
                    d2: 9233115969432897632,
                    d3: 1437907447409278328,
                }),
                tx_commitment: Some(Digest { d0: 0, d1: 0, d2: 0, d3: 0 }),
                tx_kernel_commitment: Some(Digest {
                    d0: 18367902081822286911,
                    d1: 4070978272357376021,
                    d2: 15269553592507862217,
                    d3: 10235934139480953355,
                }),
                proof_commitment: Some(Digest { d0: 0, d1: 0, d2: 0, d3: 0 }),
                timestamp: 1746737038,
            }),
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
}

pub async fn serve_stub(endpoint: &Url) -> Result<(), ApiError> {
    let addr = endpoint
        .socket_addrs(|| None)
        .map_err(ApiError::EndpointToSocketFailed)?
        .into_iter()
        .next()
        .unwrap();

    let listener = TcpListener::bind(addr).await?;
    let api_service = api_server::ApiServer::new(StubRpcApi);

    tonic::transport::Server::builder()
        .accept_http1(true)
        .add_service(tonic_web::enable(api_service))
        .serve_with_incoming(TcpListenerStream::new(listener))
        .await
        .map_err(ApiError::ApiServeFailed)
}
