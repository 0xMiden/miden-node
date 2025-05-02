use anyhow::Context;
use axum::{
    Json,
    extract::State,
    http::{Response, StatusCode},
    response::IntoResponse,
};
use http::header;
use http_body_util::Full;
use miden_node_utils::errors::ErrorReport;
use miden_objects::{
    account::AccountId,
    note::{NoteDetails, NoteExecutionMode, NoteFile, NoteId, NoteTag},
    utils::serde::Serializable,
};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tonic::body;
use tracing::info;

use crate::{COMPONENT, client::MintRequest, errors::HandlerError, state::ServerState};

/// Used to receive the initial request from the user.
///
/// Further parsing is done to get the expected [`MintRequest`] expected by the faucet client.
#[derive(Deserialize)]
struct RawMintRequest {
    account_id: String,
    is_private_note: bool,
    asset_amount: u64,
}

pub async fn get_tokens(
    State(state): State<ServerState>,
    Json(req): Json<RawMintRequest>,
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
