use axum::Json;
use axum::extract::State;
use axum::response::{Html, IntoResponse, Response};
use axum_extra::response::{Css, JavaScript};
use http::header::{self};

use crate::faucet::FaucetId;
use crate::types::AssetOptions;

/// Describes the faucet metadata.
///
/// More specifically, the faucet's account ID and allowed mint amounts.
#[derive(Clone, serde::Serialize)]
pub struct Metadata {
    pub id: FaucetId,
    pub asset_amount_options: AssetOptions,
}

pub async fn get_index_html() -> Html<&'static str> {
    Html(include_str!("resources/index.html"))
}

pub async fn get_index_js() -> JavaScript<&'static str> {
    JavaScript(include_str!("resources/index.js"))
}

pub async fn get_index_css() -> Css<&'static str> {
    Css(include_str!("resources/index.css"))
}

pub async fn get_background() -> Response {
    (
        [(header::CONTENT_TYPE, header::HeaderValue::from_static("image/png"))],
        include_bytes!("resources/background.png"),
    )
        .into_response()
}

pub async fn get_favicon() -> Response {
    (
        [(header::CONTENT_TYPE, header::HeaderValue::from_static("image/x-icon"))],
        include_bytes!("resources/favicon.ico"),
    )
        .into_response()
}

pub async fn get_metadata(State(metadata): State<&'static Metadata>) -> Json<&'static Metadata> {
    Json(metadata)
}
