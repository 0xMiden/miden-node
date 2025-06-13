use tokio::net::TcpListener;

use crate::generated::{api_server::ApiServer, status::status_api_server::StatusApiServer};

pub(crate) mod prover;
mod status;

pub use prover::{ProofType, ProverRpcApi};

pub struct RpcListener {
    pub api_service: ApiServer<ProverRpcApi>,
    pub status_service: StatusApiServer<status::StatusRpcApi>,
    pub listener: TcpListener,
}

impl RpcListener {
    pub fn new(listener: TcpListener, proof_type: ProofType) -> Self {
        let prover_rpc_api = ProverRpcApi::new(proof_type);
        let status_rpc_api = status::StatusRpcApi::new(proof_type);
        let api_service = ApiServer::new(prover_rpc_api);
        let status_service = StatusApiServer::new(status_rpc_api);
        Self { api_service, status_service, listener }
    }
}
