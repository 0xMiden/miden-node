use anyhow::Context;
use axum::{
    Json,
    extract::State,
    http::{Response, StatusCode},
    response::IntoResponse,
};
use http::header;
use http_body_util::Full;
use miden_objects::{
    account::AccountId,
    note::{NoteDetails, NoteExecutionMode, NoteFile, NoteId, NoteTag},
    utils::serde::Serializable,
};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tonic::body;
use tracing::info;

use crate::{COMPONENT, errors::HandlerError, state::FaucetState};

#[derive(Deserialize)]
pub struct FaucetRequest {
    #[serde(deserialize_with = "deserialize_account_id")]
    pub account_id: AccountId,
    pub is_private_note: bool,
    pub asset_amount: u64,
}

/// Serde deserializing helper wrapper for [`AccountId::from_hex`].
fn deserialize_account_id<'de, D>(deserializer: D) -> Result<AccountId, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let buf = String::deserialize(deserializer)?;
    AccountId::from_hex(&buf).map_err(serde::de::Error::custom)
}

#[derive(Serialize)]
pub struct FaucetMetadataReponse {
    id: String,
    asset_amount_options: Vec<u64>,
}

pub async fn get_metadata(
    State(state): State<FaucetState>,
) -> (StatusCode, Json<FaucetMetadataReponse>) {
    let response = FaucetMetadataReponse {
        id: state.account_id.to_string(),
        asset_amount_options: state.config.asset_amount_options.clone(),
    };

    (StatusCode::OK, Json(response))
}

pub async fn get_tokens(
    State(state): State<FaucetState>,
    Json(req): Json<FaucetRequest>,
) -> Result<impl IntoResponse, HandlerError> {
    let target_account = req.account_id;
    info!(
        target: COMPONENT,
        account_id = %req.account_id,
        is_private_note = %req.is_private_note,
        asset_amount = %req.asset_amount,
        "Received a request",
    );

    // Check that the amount is in the asset amount options
    if !state.config.asset_amount_options.contains(&req.asset_amount) {
        return Err(HandlerError::InvalidAssetAmount {
            requested: req.asset_amount,
            options: state.config.asset_amount_options.clone(),
        });
    }

    let (tx_result, rx_result) = oneshot::channel();

    if let Err(err) = state.request_sender.try_send((req, tx_result)) {
        todo!("handle and map errors here");
    }

    let Ok((block_height, note)) = rx_result.await else {
        todo!("return internal error");
    };

    let note_id: NoteId = note.id();
    let note_details = NoteDetails::new(note.assets().clone(), note.recipient().clone());
    // SAFETY: NoteTag creation can only error for network execution mode.
    let note_tag = NoteTag::from_account_id(target_account, NoteExecutionMode::Local).unwrap();

    // Serialize note into bytes
    let bytes = NoteFile::NoteDetails {
        details: note_details,
        after_block_num: block_height,
        tag: Some(note_tag),
    }
    .to_bytes();

    info!(target: COMPONENT, %note_id, "A new note has been created");

    // Send generated note to user
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_DISPOSITION, "attachment; filename=note.mno")
        .header("Note-Id", note_id.to_string())
        .body(body::boxed(Full::from(bytes)))
        .context("Failed to build response")
        .map_err(Into::into)
}

pub async fn get_index_html(state: State<FaucetState>) -> Result<impl IntoResponse, HandlerError> {
    get_static_file(state, "index.html")
}

pub async fn get_index_js(state: State<FaucetState>) -> Result<impl IntoResponse, HandlerError> {
    get_static_file(state, "index.js")
}

pub async fn get_index_css(state: State<FaucetState>) -> Result<impl IntoResponse, HandlerError> {
    get_static_file(state, "index.css")
}

pub async fn get_background(state: State<FaucetState>) -> Result<impl IntoResponse, HandlerError> {
    get_static_file(state, "background.png")
}

pub async fn get_favicon(state: State<FaucetState>) -> Result<impl IntoResponse, HandlerError> {
    get_static_file(state, "favicon.ico")
}

/// Returns a static file bundled with the app state.
///
/// # Panics
///
/// Panics if the file does not exist.
fn get_static_file(
    State(state): State<FaucetState>,
    file: &'static str,
) -> Result<impl IntoResponse, HandlerError> {
    info!(target: COMPONENT, file, "Serving static file");

    let static_file = state.static_files.get(file).expect("static file not found");

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, static_file.mime_type)
        .body(body::boxed(Full::from(static_file.data)))
        .context("Failed to build response")
        .map_err(Into::into)
}
