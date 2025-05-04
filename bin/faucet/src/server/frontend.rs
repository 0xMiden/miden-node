use axum::{
    Json,
    extract::State,
    response::{Html, IntoResponse, Response},
};
use http::header::{self};

use crate::{faucet::FaucetId, types::AssetOptions};

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

pub async fn get_index_js() -> Response {
    (
        [(header::CONTENT_TYPE, "Content-Type: text/javascript; charset=utf-8")],
        include_str!("resources/index.js"),
    )
        .into_response()
}

pub async fn get_index_css() -> Response {
    (
        [(header::CONTENT_TYPE, "Content-Type: text/css; charset=utf-8")],
        include_str!("resources/index.css"),
    )
        .into_response()
}

pub async fn get_background() -> Response {
    (
        [(header::CONTENT_TYPE, "Content-Type: image/png")],
        include_bytes!("resources/background.png"),
    )
        .into_response()
}

pub async fn get_favicon() -> Response {
    (
        [(header::CONTENT_TYPE, "Content-Type: image/x-icon")],
        include_bytes!("resources/favicon.ico"),
    )
        .into_response()
}

pub async fn get_metadata(State(metadata): State<&'static Metadata>) -> Json<&'static Metadata> {
    Json(metadata)
}
