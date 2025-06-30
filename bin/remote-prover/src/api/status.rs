use tonic::{Request, Response, Status};

use crate::{
    api::prover::ProofType,
    generated::{
        WorkerStatusRequest, WorkerStatusResponse, worker_status_api_server::WorkerStatusApi,
    },
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
impl WorkerStatusApi for StatusRpcApi {
    async fn status(
        &self,
        _: Request<WorkerStatusRequest>,
    ) -> Result<Response<WorkerStatusResponse>, Status> {
        Ok(Response::new(WorkerStatusResponse {
            version: env!("CARGO_PKG_VERSION").to_string(),
            supported_proof_type: self.proof_type as i32,
        }))
    }
}
