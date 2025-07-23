use std::net::TcpListener;

use http::HeaderMap;
use miden_remote_prover::error::RemoteProverError;
use pingora::{Error, ErrorType, http::ResponseHeader, protocols::http::ServerSession};
use pingora_proxy::Session;
use prost::Message;
use tonic::Code;
use tracing::debug;

use crate::{COMPONENT, commands::PROXY_HOST, proxy::metrics::QUEUE_DROP_COUNT};

/// Build gRPC trailers with status and optional message
fn build_grpc_trailers(
    grpc_status: Code,
    error_message: Option<&str>,
) -> pingora_core::Result<HeaderMap> {
    let mut trailers = HeaderMap::new();

    // Set gRPC status
    let status_code = (grpc_status as i32).to_string();
    trailers.insert(
        "grpc-status",
        status_code.parse().map_err(|e| {
            Error::new(ErrorType::InternalError)
                .more_context(format!("Failed to parse grpc-status: {e}"))
        })?,
    );

    // Set gRPC message if provided
    if let Some(message) = error_message {
        trailers.insert(
            "grpc-message",
            message.parse().map_err(|e| {
                Error::new(ErrorType::InternalError)
                    .more_context(format!("Failed to parse grpc-message: {e}"))
            })?,
        );
    }

    Ok(trailers)
}

/// Write a protobuf message as a gRPC response to a Pingora session
///
/// This helper function takes a protobuf message and writes it to a Pingora session
/// in the proper gRPC format, handling message encoding, headers, and trailers.
pub async fn write_grpc_response_to_session<T>(
    session: &mut Session,
    message: T,
) -> pingora_core::Result<bool>
where
    T: Message,
{
    // Serialize the protobuf message
    let mut response_body = Vec::new();
    message.encode(&mut response_body).map_err(|e| {
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

    session.set_keepalive(None);
    session.write_response_header(Box::new(header), false).await?;
    session.write_response_body(Some(grpc_message.into()), false).await?;

    // Send trailers with gRPC status
    let trailers = build_grpc_trailers(Code::Ok, None)?;
    session.write_response_trailers(trailers).await?;

    Ok(true)
}

/// Write a gRPC error response to a Pingora session
///
/// This helper function creates a proper gRPC error response with the specified
/// status code and error message.
pub async fn write_grpc_error_to_session(
    session: &mut Session,
    grpc_status: Code,
    error_message: &str,
) -> pingora_core::Result<bool> {
    // Create gRPC response headers (always HTTP 200 for gRPC)
    let mut header = ResponseHeader::build(200, None)?;
    header.insert_header("content-type", "application/grpc".to_string())?;

    session.set_keepalive(None);
    session.write_response_header(Box::new(header), false).await?;

    // gRPC errors don't have a body, just headers and trailers
    session.write_response_body(None, false).await?;

    // Send trailers with gRPC status and error message
    let trailers = build_grpc_trailers(grpc_status, Some(error_message))?;
    session.write_response_trailers(trailers).await?;

    Ok(true)
}

/// Create a gRPC `RESOURCE_EXHAUSTED` response for a full queue
pub(crate) async fn create_queue_full_response(
    session: &mut Session,
) -> pingora_core::Result<bool> {
    // Increment the queue drop count metric
    QUEUE_DROP_COUNT.inc();

    // Use our helper function to create a proper gRPC error response
    write_grpc_error_to_session(session, Code::ResourceExhausted, "Too many requests in the queue")
        .await
}

/// Create a gRPC `RESOURCE_EXHAUSTED` response for rate limiting
pub async fn create_too_many_requests_response(
    session: &mut Session,
    max_request_per_second: isize,
) -> pingora_core::Result<bool> {
    // Use our helper function to create a proper gRPC error response
    let error_message =
        format!("Rate limit exceeded: {max_request_per_second} requests per second");
    write_grpc_error_to_session(session, Code::ResourceExhausted, &error_message).await
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
