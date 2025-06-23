use std::net::TcpListener;

use pingora::{Error, ErrorType, http::ResponseHeader, protocols::http::ServerSession};
use pingora_proxy::Session;
use tracing::debug;

use crate::{
    COMPONENT, commands::PROXY_HOST, error::ProvingServiceError, proxy::metrics::QUEUE_DROP_COUNT,
};

const RESOURCE_EXHAUSTED_CODE: u16 = 8;

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
/// * `Err(ProvingServiceError::PortAlreadyInUse)` if the port is already in use.
pub fn check_port_availability(
    port: u16,
    service: &str,
) -> Result<std::net::TcpListener, ProvingServiceError> {
    let addr = format!("{PROXY_HOST}:{port}");
    TcpListener::bind(&addr)
        .inspect(|_| debug!(target: COMPONENT, %service, %port, %addr, "Port is available"))
        .map_err(|err| ProvingServiceError::PortAlreadyInUse(err, port))
}
