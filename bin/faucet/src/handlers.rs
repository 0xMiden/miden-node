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
use tonic::body;
use tracing::info;
use tracing::instrument;

use crate::{COMPONENT, errors::HandlerError, state::FaucetState};

#[derive(Deserialize)]
pub struct FaucetRequest {
    account_id: String,
    is_private_note: bool,
    asset_amount: u64,
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
        id: state.id.to_string(),
        asset_amount_options: state.config.asset_amount_options.clone(),
    };

    (StatusCode::OK, Json(response))
}

#[instrument(
    parent = None, target = COMPONENT, name = "faucet.server.get_tokens",
    skip_all, err,
    fields(
        account_id = %req.account_id,
        is_private_note = %req.is_private_note,
        asset_amount = %req.asset_amount,
    )
)]
pub async fn get_tokens(
    State(state): State<FaucetState>,
    Json(req): Json<FaucetRequest>,
) -> Result<impl IntoResponse, HandlerError> {
    // Check that the amount is in the asset amount options
    if !state.config.asset_amount_options.contains(&req.asset_amount) {
        return Err(HandlerError::InvalidAssetAmount {
            requested: req.asset_amount,
            options: state.config.asset_amount_options.clone(),
        });
    }

    let mut client = state.client.lock().await;

    // Receive and hex user account id
    let target_account_id = AccountId::from_hex(req.account_id.as_str())
        .map_err(HandlerError::AccountIdDeserializationError)?;

    // Execute transaction
    info!(target: COMPONENT, "Executing mint transaction for account.");
    let (executed_tx, created_note) = client.execute_mint_transaction(
        target_account_id,
        req.is_private_note,
        req.asset_amount,
    )?;

    let mut faucet_account = client.data_store().faucet_account();
    faucet_account
        .apply_delta(executed_tx.account_delta())
        .context("Failed to apply faucet account delta")?;

    // Run transaction prover & send transaction to node
    info!(target: COMPONENT, "Proving and submitting transaction.");
    let block_height = client.prove_and_submit_transaction(executed_tx).await?;

    // Update data store with the new faucet state
    client.data_store().update_faucet_state(faucet_account);

    let note_id: NoteId = created_note.id();
    let note_details =
        NoteDetails::new(created_note.assets().clone(), created_note.recipient().clone());

    let note_tag = NoteTag::from_account_id(target_account_id, NoteExecutionMode::Local)
        .context("failed to build note tag for local execution")?;

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
