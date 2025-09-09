use axum::Router;
use axum::response::Html;
use axum::routing::get;
use tower_http::services::ServeDir;

use crate::status::{MonitoringConfig, SharedStatus};

/// Runs the frontend server.
///
/// This function runs the frontend server that serves the dashboard and the status data.
///
/// # Arguments
///
/// * `shared_status` - The shared status of the network.
/// * `config` - The configuration of the network.
pub async fn run_frontend(shared_status: SharedStatus, config: MonitoringConfig) {
    // build our application with routes
    let app = Router::new()
        // Serve static files from assets directory
        .nest_service("/assets", ServeDir::new("bin/network-monitoring/assets"))
        // Main dashboard route
        .route("/", get(get_dashboard))
        // API route for status data
        .route("/status", get(get_status))
        .with_state(shared_status);

    // run our app with hyper, listening globally on the configured port
    let bind_address = format!("0.0.0.0:{}", config.port);
    println!("Starting web server on {bind_address}");
    println!("Dashboard available at: http://localhost:{}/", config.port);
    let listener = tokio::net::TcpListener::bind(&bind_address).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

pub async fn get_dashboard() -> Html<&'static str> {
    Html(include_str!("../assets/index.html"))
}

pub async fn get_status(
    axum::extract::State(shared_status): axum::extract::State<SharedStatus>,
) -> axum::response::Json<crate::status::NetworkStatus> {
    let status = shared_status.lock().await;
    axum::response::Json(status.clone())
}
