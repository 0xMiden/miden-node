use anyhow::Context;
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
        GetBlockHeaderByNumberResponse, GetNotesByIdResponse, RpcStatusResponse,
        SubmitProvenTransactionResponse, SyncNoteResponse, SyncStateResponse,
    },
    rpc::api_server,
};
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
                    d0: 0x9729_9D39_2DA8_DC69,
                    d1: 0x0674_44AF_6294_0719,
                    d2: 0x7B97_0BC7_07A0_F7D6,
                    d3: 0xE423_8D7C_78F3_9D8B,
                }),
                account_root: Some(Digest {
                    d0: 0x8376_1091_8A4C_E7C0,
                    d1: 0xA569_B0BE_10ED_30AC,
                    d2: 0x73EA_6699_EC30_7B1B,
                    d3: 0xC95C_3F75_1F0A_AEC8,
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
                    d0: 0xFEE7_E5A0_482C_E43F,
                    d1: 0x387F_052B_7431_E015,
                    d2: 0xD3E8_5A17_4038_8CC9,
                    d3: 0x8E0D_57DE_180E_520B,
                }),
                proof_commitment: Some(Digest { d0: 0, d1: 0, d2: 0, d3: 0 }),
                timestamp: 1_746_737_038,
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
        .add_service(tonic_web::enable(api_service)) // tonic_web::enable is needed to support grpc-web calls
        .serve_with_incoming(TcpListenerStream::new(listener))
        .await
        .context("failed to serve stub RPC API")
}
