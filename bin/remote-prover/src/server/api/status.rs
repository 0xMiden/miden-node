use tonic::{Request, Response, Status};

use crate::generated::server::worker_status_api_server::WorkerStatusApi;
use crate::generated::{self as proto};
use crate::server::api::prover::ProofType;

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
    async fn status(&self, _: Request<()>) -> Result<Response<proto::WorkerStatus>, Status> {
        Ok(Response::new(proto::WorkerStatus {
            version: env!("CARGO_PKG_VERSION").to_string(),
            supported_proof_type: self.proof_type as i32,
        }))
    }
}
