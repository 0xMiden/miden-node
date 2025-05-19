use std::convert::Infallible;

use axum::response::sse::Event;
use base64::{Engine, engine::general_purpose};
use miden_objects::{
    account::AccountId,
    block::BlockNumber,
    note::{Note, NoteDetails, NoteExecutionMode, NoteFile, NoteTag, NoteType},
    transaction::TransactionId,
    utils::Serializable,
};
use tokio::sync::mpsc::Sender;

use crate::{EXPLORER_URL, NETWORK_ID};

pub type ResponseSender = Sender<Result<Event, Infallible>>;

/// Sends updates on the minting process to all the clients waiting for a batch of mint requests to
/// be processed.
pub struct ClientUpdater {
    clients: Vec<ResponseSender>,
}

impl ClientUpdater {
    /// Creates a new client updater.
    pub fn new(clients: Vec<ResponseSender>) -> Self {
        Self { clients }
    }

    /// Sends an update to all the batch clients.
    /// Errors when sending through the channel are ignored since the client may have cancelled the
    /// request.
    pub async fn send_updates(&self, update: MintUpdate) {
        let event = update.into_event();
        for sender in &self.clients {
            let _ = sender.send(Ok(event.clone())).await;
        }
    }

    /// Sends a serialized note to all the batch clients. Each note is sent to the corresponding
    /// client.
    /// Errors when sending through the channel are ignored since the client may have cancelled the
    /// request.
    pub async fn send_notes(
        &self,
        block_number: BlockNumber,
        notes: &[Note],
        tx_id: TransactionId,
    ) {
        for (note, sender) in notes.iter().zip(&self.clients) {
            let _ = sender
                .send(Ok(MintUpdate::Minted(note.clone(), block_number, tx_id).into_event()))
                .await;
        }
    }
}

/// The different stages of the minting process.
#[allow(clippy::large_enum_variant)]
pub enum MintUpdate {
    // TODO: add PoW verification event
    Built,
    Executed,
    Proven,
    Submitted,
    Minted(Note, BlockNumber, TransactionId),
}

impl MintUpdate {
    /// Converts the mint update into an sse event.
    /// Event types:
    /// - `MintUpdate::Built`: event type "update"
    /// - `MintUpdate::Executed`: event type "update"
    /// - `MintUpdate::Proven`: event type "update"
    /// - `MintUpdate::Submitted`: event type "update"
    /// - `MintUpdate::Minted`: event type "note". Contains the note encoded in base64 if it is
    ///   private.
    pub fn into_event(self) -> Event {
        match self {
            MintUpdate::Minted(note, block_height, tx_id) => {
                let note_id = note.id();
                let note_details =
                    NoteDetails::new(note.assets().clone(), note.recipient().clone());
                // SAFETY: in a valid p2id note, the account id is the encoded in the first two note
                // inputs
                let account_id =
                    AccountId::try_from([note.inputs().values()[1], note.inputs().values()[0]])
                        .unwrap();
                let note_tag = NoteTag::from_account_id(account_id, NoteExecutionMode::Local).ok();

                // If the note is private, encode the note bytes as a base64 string
                let bytes = if note.metadata().note_type() == NoteType::Private {
                    NoteFile::NoteDetails {
                        details: note_details,
                        after_block_num: block_height,
                        tag: note_tag,
                    }
                    .to_bytes()
                } else {
                    Vec::new()
                };
                let encoded_note = general_purpose::STANDARD.encode(&bytes);

                let event_payload = serde_json::json!({
                    "note_id": note_id.to_string(),
                    "account_id": account_id.to_bech32(NETWORK_ID),
                    "transaction_id": tx_id.to_string(),
                    "explorer_url": EXPLORER_URL,
                    "data_base64": encoded_note,
                });

                Event::default().event("note").data(event_payload.to_string())
            },
            MintUpdate::Built => Event::default().event("update").data("Built"),
            MintUpdate::Executed => Event::default().event("update").data("Executed"),
            MintUpdate::Proven => Event::default().event("update").data("Proven"),
            MintUpdate::Submitted => Event::default().event("update").data("Submitted"),
        }
    }
}
