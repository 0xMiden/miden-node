//! This file explicitly embeds each of the frontend files into the binary using `include_str!` and
//! `include_bytes!`.

use anyhow::Context;
use axum::Router;
use axum::http::header;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;

use crate::status::{MonitorConfig, SharedStatus};

/// Runs the frontend server.
///
/// This function runs the frontend server that serves the dashboard and the status data.
///
/// # Arguments
///
/// * `shared_status` - The shared status of the network.
/// * `config` - The configuration of the network.
pub async fn serve(shared_status: SharedStatus, config: MonitorConfig) -> anyhow::Result<()> {
    // build our application with routes
    let app = Router::new()
        // Serve embedded assets
        .route("/assets/index.css", get(serve_css))
        .route("/assets/favicon.ico", get(serve_favicon))
        // Main dashboard route
        .route("/", get(get_dashboard))
        // API route for status data
        .route("/status", get(get_status))
        .with_state(shared_status);

    let bind_address = format!("0.0.0.0:{}", config.port);
    println!("Starting web server on {bind_address}");
    println!("Dashboard available at: http://localhost:{}/", config.port);
    let listener = tokio::net::TcpListener::bind(&bind_address)
        .await
        .context("Failed to bind to address")?;
    axum::serve(listener, app).await.context("Failed to start web server")?;
    Ok(())
}

async fn get_dashboard() -> Html<&'static str> {
    Html(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/index.html")))
}

async fn get_status(
    axum::extract::State(shared_status): axum::extract::State<SharedStatus>,
) -> axum::response::Json<crate::status::NetworkStatus> {
    let status = shared_status.lock().await;
    axum::response::Json(status.clone())
}

async fn serve_css() -> Response {
    (
        [(header::CONTENT_TYPE, header::HeaderValue::from_static("text/css"))],
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/index.css")),
    )
        .into_response()
}

async fn serve_favicon() -> Response {
    (
        [(header::CONTENT_TYPE, header::HeaderValue::from_static("image/x-icon"))],
        include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/favicon.ico")),
    )
        .into_response()
}
