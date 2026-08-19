use miden_node_proto::decode::convert_digests_to_words;
use miden_node_proto::generated as proto;
use miden_node_proto::generated::note::CommittedNote;
use miden_node_store::NoteRecord;
use miden_node_utils::limiter::QueryParamNoteIdLimit;
use miden_node_utils::tracing::miden_instrument;
use miden_protocol::Word;
use miden_protocol::note::NoteId;
use tonic::Status;

use super::{RpcService, check, database_error_to_status};
use crate::{COMPONENT, LOG_TARGET};

#[tonic::async_trait]
impl proto::server::rpc_api::GetNotesById for RpcService {
    type Input = proto::note::NoteIdList;
    type Output = Vec<CommittedNote>;

    fn decode(request: proto::note::NoteIdList) -> tonic::Result<Self::Input> {
        Ok(request)
    }

    fn encode(notes: Self::Output) -> tonic::Result<proto::note::CommittedNoteList> {
        Ok(proto::note::CommittedNoteList { notes })
    }

    #[miden_instrument(
        target = COMPONENT,
        name = "get_notes_by_id",
        err,
    )]
    async fn handle(
        &self,
        request: Self::Input,
        _metadata: &tonic::metadata::MetadataMap,
        _extensions: &tonic::codegen::http::Extensions,
    ) -> tonic::Result<Self::Output> {
        tracing::trace!(target: LOG_TARGET, ?request);

        check::<QueryParamNoteIdLimit>(request.ids.len())?;

        let note_ids: Vec<Word> = convert_digests_to_words::<Status, _>(request.ids)?;
        let note_ids: Vec<NoteId> = note_ids.into_iter().map(NoteId::from_raw).collect();

        let notes = self
            .state
            .view()
            .get_notes_by_id(note_ids)
            .await
            .map_err(|err| database_error_to_status(&err))?
            .into_iter()
            .map(note_record_to_proto)
            .collect();

        Ok(notes)
    }
}

// HELPERS
// ================================================================================================

fn note_record_to_proto(note: NoteRecord) -> proto::note::CommittedNote {
    let inclusion_proof = Some(proto::note::NoteInclusionInBlockProof {
        note_id: Some(note.note_id.into()),
        block_num: note.block_num.as_u32(),
        note_index_in_block: note.note_index.leaf_index_value().into(),
        inclusion_path: Some(note.inclusion_path.into()),
    });
    let note = Some(proto::note::Note {
        metadata: Some(note.metadata.into()),
        note_details: note.details.map(Into::into),
        note_attachments: Some(note.attachments.into()),
    });
    proto::note::CommittedNote { inclusion_proof, note }
}

#[cfg(test)]
mod tests {
    use miden_node_proto::prost::Message;
    use miden_node_utils::limiter::{
        MAX_RESPONSE_PAYLOAD_BYTES,
        QueryParamLimiter,
        QueryParamNoteIdLimit,
    };
    use miden_protocol::NOTE_MAX_SIZE;
    use miden_protocol::block::{BlockNoteIndex, BlockNumber};
    use miden_protocol::crypto::merkle::SparseMerklePath;
    use miden_protocol::note::{Note, NoteDetails, NoteType, PartialNoteMetadata};

    use super::*;

    fn note_record(note: Note, include_details: bool) -> NoteRecord {
        let note_id = Word::new(*note.id().as_word());
        let (assets, metadata, recipient, attachments) = note.into_parts();
        NoteRecord {
            block_num: BlockNumber::from(1),
            note_index: BlockNoteIndex::new(0, 0).unwrap(),
            note_id,
            metadata,
            details: include_details.then(|| NoteDetails::new(assets, recipient)),
            attachments,
            inclusion_path: SparseMerklePath::default(),
        }
    }

    fn public_note() -> Note {
        let note = Note::mock_noop(Word::from([1, 2, 3, 4u32]));
        let (assets, metadata, recipient, attachments) = note.into_parts();
        let partial_metadata =
            PartialNoteMetadata::new(metadata.sender(), NoteType::Public).with_tag(metadata.tag());
        Note::with_attachments(assets, partial_metadata, recipient, attachments)
    }

    fn maximum_representative_note() -> proto::note::CommittedNote {
        let digest = proto::primitives::Digest {
            d0: u64::MAX,
            d1: u64::MAX,
            d2: u64::MAX,
            d3: u64::MAX,
        };
        let attachment = proto::note::NoteAttachment {
            scheme: u32::from(u16::MAX - 1),
            words: vec![
                proto::primitives::Word {
                    encoded: vec![u8::MAX; Word::SERIALIZED_SIZE]
                };
                256
            ],
        };
        let metadata = proto::note::NoteMetadata {
            sender: Some(proto::account::AccountId { id: vec![u8::MAX; 15] }),
            note_type: proto::note::NoteType::Public as i32,
            tag: u32::MAX,
            attachment_schemes: vec![u32::from(u16::MAX - 1); 4],
            attachments_commitment: Some(digest),
        };
        let note = proto::note::Note {
            metadata: Some(metadata),
            note_details: Some(proto::note::NoteDetails {
                assets: Vec::new(),
                recipient: Some(proto::note::NoteRecipient {
                    serial_num: Some(proto::primitives::Word {
                        encoded: vec![u8::MAX; Word::SERIALIZED_SIZE],
                    }),
                    // Deliberately conservative: allow the opaque MAST leaf alone to approach the
                    // protocol's complete-note size bound.
                    script: Some(proto::note::NoteScript {
                        entrypoint: u32::MAX,
                        mast: vec![u8::MAX; NOTE_MAX_SIZE as usize],
                    }),
                    storage: Some(proto::note::NoteStorage {
                        items: vec![
                            proto::primitives::Felt {
                                encoded: vec![u8::MAX; size_of::<u64>()],
                            };
                            miden_protocol::MAX_NOTE_STORAGE_ITEMS
                        ],
                    }),
                }),
            }),
            note_attachments: Some(proto::note::NoteAttachments {
                attachments: vec![attachment; 2],
            }),
        };
        let inclusion_proof = proto::note::NoteInclusionInBlockProof {
            note_id: Some(proto::note::NoteId { id: Some(digest) }),
            block_num: u32::MAX,
            note_index_in_block: u32::MAX,
            inclusion_path: Some(proto::primitives::SparseMerklePath {
                empty_nodes_mask: u64::MAX,
                siblings: vec![digest; 64],
            }),
        };

        proto::note::CommittedNote {
            note: Some(note),
            inclusion_proof: Some(inclusion_proof),
        }
    }

    #[test]
    fn maximum_get_notes_response_fits_payload_limit() {
        let note = maximum_representative_note();
        let response = proto::note::CommittedNoteList {
            notes: vec![note.clone(); QueryParamNoteIdLimit::LIMIT],
        };
        assert!(
            response.encoded_len() <= MAX_RESPONSE_PAYLOAD_BYTES,
            "{} notes encode to {} bytes, exceeding the {} byte response limit",
            QueryParamNoteIdLimit::LIMIT,
            response.encoded_len(),
            MAX_RESPONSE_PAYLOAD_BYTES,
        );

        let response = proto::note::CommittedNoteList {
            notes: vec![note; QueryParamNoteIdLimit::LIMIT + 1],
        };
        assert!(
            response.encoded_len() > MAX_RESPONSE_PAYLOAD_BYTES,
            "the query limit can be raised without exceeding the response payload bound"
        );
    }

    #[test]
    fn private_note_response_omits_details_and_keeps_attachments() {
        let encoded =
            note_record_to_proto(note_record(Note::mock_noop(Word::from([5, 6, 7, 8u32])), false));
        let note = encoded.note.unwrap();

        assert!(note.note_details.is_none());
        assert!(note.note_attachments.is_some());
    }

    #[test]
    fn public_note_response_contains_structured_details() {
        let original = public_note();
        let expected_commitment = original.details_commitment();
        let encoded = note_record_to_proto(note_record(original, true));
        let note = encoded.note.unwrap();

        let details = NoteDetails::try_from(note.note_details.unwrap()).unwrap();
        assert_eq!(details.commitment(), expected_commitment);
        assert!(note.note_attachments.is_some());
    }
}
