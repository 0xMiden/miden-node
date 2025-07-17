use std::net::TcpListener;

use http::HeaderMap;
use miden_remote_prover::{
    error::RemoteProverError,
    generated::remote_prover::{ProxyStatusResponse, WorkerStatus},
};
use pingora::{Error, ErrorType, http::ResponseHeader, protocols::http::ServerSession};
use pingora_proxy::Session;
use prost::Message;
use tracing::debug;

use crate::{
    COMPONENT,
    commands::PROXY_HOST,
    proxy::{LoadBalancerState, metrics::QUEUE_DROP_COUNT},
};

const RESOURCE_EXHAUSTED_CODE: u16 = 8;

/// Create a gRPC proxy status response
///
/// This creates a proper gRPC response with the `ProxyStatusResponse` serialized as protobuf
pub async fn create_proxy_status_response(
    session: &mut Session,
    load_balancer_state: &LoadBalancerState,
) -> pingora_core::Result<bool> {
    // Build the proxy status response
    let version = env!("CARGO_PKG_VERSION").to_string();
    let supported_proof_type: i32 = load_balancer_state.supported_proof_type().into();

    let workers = load_balancer_state.workers().await;
    let worker_statuses: Vec<WorkerStatus> = workers.iter().map(WorkerStatus::from).collect();

    let status_response = ProxyStatusResponse {
        version,
        supported_proof_type,
        workers: worker_statuses,
    };

    // Serialize the protobuf message
    let mut response_body = Vec::new();
    status_response.encode(&mut response_body).map_err(|e| {
        Error::new(ErrorType::InternalError)
            .more_context(format!("Failed to encode proto response: {e}"))
    })?;

    let mut grpc_message = Vec::new();

    // Add compression flag (1 byte, 0 = no compression)
    grpc_message.push(0u8);

    // Add message length (4 bytes, big-endian)
    let msg_len = response_body.len() as u32;
    grpc_message.extend_from_slice(&msg_len.to_be_bytes());

    // Add the actual message
    grpc_message.extend_from_slice(&response_body);

    // Create gRPC response headers WITHOUT grpc-status (that goes in trailers)
    let mut header = ResponseHeader::build(200, None)?;
    header.insert_header("content-type", "application/grpc".to_string())?;
    // Don't set grpc-status here - it must be in trailers for proper gRPC

    session.set_keepalive(None);
    session.write_response_header(Box::new(header), false).await?;
    session.write_response_body(Some(grpc_message.into()), false).await?;

    // Send trailers with gRPC status
    let mut trailers = HeaderMap::new();
    trailers.insert("grpc-status", "0".parse().map_err(|_| Error::new(ErrorType::InternalError))?);
    session.write_response_trailers(trailers).await?;

    Ok(true)
}

/// Create a 503 response for a full queue
pub(crate) async fn create_queue_full_response(
    session: &mut Session,
) -> pingora_core::Result<bool> {
    // Set grpc-message header to "Too many requests in the queue"
    // This is meant to be used by a Tonic interceptor to return a gRPC error
    let mut header = ResponseHeader::build(503, None)?;
    header.insert_header("grpc-message", "Too many requests in the queue".to_string())?;
    header.insert_header("grpc-status", RESOURCE_EXHAUSTED_CODE)?;
    session.set_keepalive(None);
    session.write_response_header(Box::new(header.clone()), true).await?;

    let mut error = Error::new(ErrorType::HTTPStatus(503))
        .more_context("Too many requests in the queue")
        .into_in();
    error.set_cause("Too many requests in the queue");

    session.write_response_header(Box::new(header), false).await?;

    // Increment the queue drop count metric
    QUEUE_DROP_COUNT.inc();

    Err(error)
}

/// Create a 429 response for too many requests
pub async fn create_too_many_requests_response(
    session: &mut Session,
    max_request_per_second: isize,
) -> pingora_core::Result<bool> {
    // Rate limited, return 429
    let mut header = ResponseHeader::build(429, None)?;
    header.insert_header("X-Rate-Limit-Limit", max_request_per_second.to_string())?;
    header.insert_header("X-Rate-Limit-Remaining", "0")?;
    header.insert_header("X-Rate-Limit-Reset", "1")?;
    session.set_keepalive(None);
    session.write_response_header(Box::new(header), true).await?;
    Ok(true)
}

/// Create a 400 response with an error message
///
/// It will set the X-Error-Message header to the error message.
pub async fn create_response_with_error_message(
    session: &mut ServerSession,
    error_msg: String,
) -> pingora_core::Result<bool> {
    let mut header = ResponseHeader::build(400, None)?;
    header.insert_header("X-Error-Message", error_msg)?;
    session.set_keepalive(None);
    session.write_response_header(Box::new(header)).await?;
    Ok(true)
}

/// Checks if a port is available for use.
///
/// # Arguments
/// * `port` - The port to check.
/// * `service` - A descriptive name for the service (for logging purposes).
///
/// # Returns
/// * `Ok(TcpListener)` if the port is available.
/// * `Err(RemoteProverError::PortAlreadyInUse)` if the port is already in use.
pub fn check_port_availability(
    port: u16,
    service: &str,
) -> Result<std::net::TcpListener, RemoteProverError> {
    let addr = format!("{PROXY_HOST}:{port}");
    TcpListener::bind(&addr)
        .inspect(|_| debug!(target: COMPONENT, %service, %port, %addr, "Port is available"))
        .map_err(|err| RemoteProverError::PortAlreadyInUse(err, port))
}
