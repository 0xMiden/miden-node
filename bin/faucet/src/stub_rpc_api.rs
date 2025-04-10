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
        // Values are taken from the default genesis block as at v0.7
        Ok(Response::new(GetBlockHeaderByNumberResponse {
            block_header: Some(BlockHeader {
                version: 1,
                prev_block_commitment: Some(Digest { d0: 0, d1: 0, d2: 0, d3: 0 }),
                block_num: 0,
                chain_commitment: Some(Digest {
                    d0: 0x9729_9D39_2DA8_DC69,
                    d1: 0x674_44AF_6294_0719,
                    d2: 0x7B97_0BC7_07A0_F7D6,
                    d3: 0xE423_8D7C_78F3_9D8B,
                }),
                account_root: Some(Digest {
                    d0: 0x9666_5D75_8487_401A,
                    d1: 0xB7BF_DF8B_379F_ED71,
                    d2: 0xFCA7_82CB_2406_2222,
                    d3: 0x8D0C_B80F_6377_4E9A,
                }),
                nullifier_root: Some(Digest {
                    d0: 0xD4A0_CFF6_578C_123E,
                    d1: 0xF11A_1794_8930_B14A,
                    d2: 0xD128_DD2A_4213_B53C,
                    d3: 0x2DF8_FE54_F23F_6B91,
                }),
                note_root: Some(Digest {
                    d0: 0x93CE_DDC8_A187_24FE,
                    d1: 0x4E32_9917_2E91_30ED,
                    d2: 0x8022_9E0E_1808_C860,
                    d3: 0x13F4_7934_7EB7_FD78,
                }),
                tx_commitment: Some(Digest { d0: 0, d1: 0, d2: 0, d3: 0 }),
                tx_kernel_commitment: Some(Digest {
                    d0: 0x7B6F_43E5_2910_C8C3,
                    d1: 0x99B3_2868_577E_5779,
                    d2: 0xAF9E_6424_57CD_B8C1,
                    d3: 0xB1DD_E61B_F983_2DBD,
                }),
                proof_commitment: Some(Digest { d0: 0, d1: 0, d2: 0, d3: 0 }),
                timestamp: 0x63B0_CD00,
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
