use tonic::{Request, Response, Status};

use crate::{
    api::prover::ProofType,
    generated::{StatusRequest, StatusResponse, status_api_server::StatusApi},
};

pub struct StatusRpcApi {
    proof_type: ProofType,
}

impl StatusRpcApi {
    pub fn new(proof_type: ProofType) -> Self {
        Self { proof_type }
    }
}

#[async_trait::async_trait]
impl StatusApi for StatusRpcApi {
    async fn status(&self, _: Request<StatusRequest>) -> Result<Response<StatusResponse>, Status> {
        Ok(Response::new(StatusResponse {
            version: env!("CARGO_PKG_VERSION").to_string(),
            supported_proof_type: self.proof_type as i32,
        }))
    }
}
