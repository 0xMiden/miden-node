use std::sync::Arc;

use miden_protocol::crypto::merkle::SparseMerklePath;
use miden_protocol::note::{
    Note,
    NoteAttachment,
    NoteAttachmentHeader,
    NoteAttachmentScheme,
    NoteAttachments,
    NoteDetails,
    NoteDetailsCommitment,
    NoteHeader,
    NoteId,
    NoteInclusionProof,
    NoteMetadata,
    NoteScript,
    NoteTag,
    NoteType,
    PartialNoteMetadata,
};
use miden_protocol::utils::serde::Serializable;
use miden_protocol::{MastForest, MastNodeId, Word};
use miden_standards::note::AccountTargetNetworkNote;

use crate::decode::{ConversionResultExt, DecodeBytesExt, GrpcDecodeExt, GrpcStructDecoder};
use crate::errors::ConversionError;
use crate::{decode, generated as proto};

// NOTE TYPE
// ================================================================================================

impl From<NoteType> for proto::note::NoteType {
    fn from(note_type: NoteType) -> Self {
        match note_type {
            NoteType::Public => proto::note::NoteType::Public,
            NoteType::Private => proto::note::NoteType::Private,
        }
    }
}

impl TryFrom<proto::note::NoteType> for NoteType {
    type Error = ConversionError;

    fn try_from(note_type: proto::note::NoteType) -> Result<Self, Self::Error> {
        match note_type {
            proto::note::NoteType::Public => Ok(NoteType::Public),
            proto::note::NoteType::Private => Ok(NoteType::Private),
            proto::note::NoteType::Unspecified => {
                Err(ConversionError::message("enum variant discriminant out of range"))
            },
        }
    }
}

// NOTE METADATA
// ================================================================================================

impl From<NoteMetadata> for proto::note::NoteMetadata {
    fn from(val: NoteMetadata) -> Self {
        let sender = Some(val.sender().into());
        let note_type = proto::note::NoteType::from(val.note_type()) as i32;
        let tag = val.tag().as_u32();
        let attachment_schemes = val
            .attachment_headers()
            .iter()
            .map(|header| u32::from(header.scheme().map_or(0, |s| s.as_u16())))
            .collect();
        let attachments_commitment = Some(val.attachments_commitment().into());

        proto::note::NoteMetadata {
            sender,
            note_type,
            tag,
            attachment_schemes,
            attachments_commitment,
        }
    }
}

impl TryFrom<proto::note::NoteMetadata> for NoteMetadata {
    type Error = ConversionError;

    fn try_from(value: proto::note::NoteMetadata) -> Result<Self, Self::Error> {
        let decoder = value.decoder();
        let sender = decode!(decoder, value.sender)?;
        let note_type = proto::note::NoteType::try_from(value.note_type)
            .map_err(|_| ConversionError::message("enum variant discriminant out of range"))?
            .try_into()
            .context("note_type")?;
        let tag = NoteTag::new(value.tag);
        let attachments_commitment: Word = decode!(decoder, value.attachments_commitment)?;

        if value.attachment_schemes.len() > NoteAttachments::MAX_COUNT {
            return Err(ConversionError::message("too many attachment schemes"));
        }
        let mut attachment_headers = [NoteAttachmentHeader::absent(); NoteAttachments::MAX_COUNT];
        for (slot, raw) in attachment_headers.iter_mut().zip(value.attachment_schemes) {
            let raw = u16::try_from(raw)
                .map_err(|_| ConversionError::message("attachment scheme out of u16 range"))?;
            *slot = if raw == 0 {
                NoteAttachmentHeader::absent()
            } else {
                NoteAttachmentHeader::new(NoteAttachmentScheme::new(raw)?)
            };
        }

        let partial = PartialNoteMetadata::new(sender, note_type).with_tag(tag);
        Ok(NoteMetadata::from_parts(partial, attachment_headers, attachments_commitment))
    }
}

// NOTE
// ================================================================================================

impl From<&NoteAttachment> for proto::note::NoteAttachment {
    fn from(attachment: &NoteAttachment) -> Self {
        Self {
            scheme: u32::from(attachment.attachment_scheme().as_u16()),
            words: attachment.content().as_words().iter().map(Into::into).collect(),
        }
    }
}

impl TryFrom<proto::note::NoteAttachment> for NoteAttachment {
    type Error = ConversionError;

    fn try_from(attachment: proto::note::NoteAttachment) -> Result<Self, Self::Error> {
        let scheme = u16::try_from(attachment.scheme).context("scheme")?;
        let scheme = NoteAttachmentScheme::new(scheme)
            .map_err(ConversionError::from)
            .context("scheme")?;
        let words = attachment
            .words
            .into_iter()
            .map(Word::try_from)
            .collect::<Result<Vec<_>, _>>()
            .context("words")?;

        NoteAttachment::with_words(scheme, words)
            .map_err(ConversionError::from)
            .context("words")
    }
}

impl From<NoteAttachments> for proto::note::NoteAttachments {
    fn from(attachments: NoteAttachments) -> Self {
        Self::from(&attachments)
    }
}

impl From<&NoteAttachments> for proto::note::NoteAttachments {
    fn from(attachments: &NoteAttachments) -> Self {
        Self {
            attachments: attachments.iter().map(Into::into).collect(),
        }
    }
}

impl TryFrom<proto::note::NoteAttachments> for NoteAttachments {
    type Error = ConversionError;

    fn try_from(attachments: proto::note::NoteAttachments) -> Result<Self, Self::Error> {
        let attachments = attachments
            .attachments
            .into_iter()
            .map(NoteAttachment::try_from)
            .collect::<Result<Vec<_>, _>>()
            .context("attachments")?;

        NoteAttachments::new(attachments)
            .map_err(ConversionError::from)
            .context("attachments")
    }
}

impl From<Note> for proto::note::NetworkNote {
    fn from(note: Note) -> Self {
        let metadata = Some(proto::note::NoteMetadata::from(*note.metadata()));
        let note_attachments = Some(note.attachments().into());
        let details = NoteDetails::from(note).to_bytes();
        Self { metadata, details, note_attachments }
    }
}

impl From<Note> for proto::note::Note {
    fn from(note: Note) -> Self {
        let metadata = Some(proto::note::NoteMetadata::from(*note.metadata()));
        let note_attachments = Some(note.attachments().into());
        let details = Some(NoteDetails::from(note).to_bytes());
        Self { metadata, details, note_attachments }
    }
}

impl From<AccountTargetNetworkNote> for proto::note::NetworkNote {
    fn from(note: AccountTargetNetworkNote) -> Self {
        note.into_note().into()
    }
}

impl TryFrom<proto::note::NetworkNote> for AccountTargetNetworkNote {
    type Error = ConversionError;

    fn try_from(value: proto::note::NetworkNote) -> Result<Self, Self::Error> {
        let decoder = value.decoder();
        let proto::note::NetworkNote { metadata, details, note_attachments } = value;

        let metadata = decode!(decoder, metadata)?;
        let partial_metadata = partial_note_metadata_from_proto(metadata)?;

        let note_details = NoteDetails::decode_bytes(&details, "NoteDetails")?;
        let (assets, recipient) = note_details.into_parts();
        let attachments = decode_note_attachments::<proto::note::NetworkNote>(note_attachments)?;

        let note = Note::with_attachments(assets, partial_metadata, recipient, attachments);
        AccountTargetNetworkNote::new(note).map_err(ConversionError::from)
    }
}

impl TryFrom<proto::note::Note> for Note {
    type Error = ConversionError;

    fn try_from(proto_note: proto::note::Note) -> Result<Self, Self::Error> {
        let decoder = proto_note.decoder();
        let proto::note::Note { metadata, details, note_attachments } = proto_note;

        let metadata = decode!(decoder, metadata)?;
        let partial_metadata = partial_note_metadata_from_proto(metadata)?;

        let details: Vec<u8> = decode!(decoder, details)?;
        let note_details = NoteDetails::decode_bytes(&details, "NoteDetails")?;
        let (assets, recipient) = note_details.into_parts();
        let attachments = decode_note_attachments::<proto::note::Note>(note_attachments)?;

        Ok(Note::with_attachments(assets, partial_metadata, recipient, attachments))
    }
}

// NOTE ID
// ================================================================================================

impl From<Word> for proto::note::NoteId {
    fn from(digest: Word) -> Self {
        Self { id: Some(digest.into()) }
    }
}

impl TryFrom<proto::note::NoteId> for Word {
    type Error = ConversionError;

    fn try_from(note_id: proto::note::NoteId) -> Result<Self, Self::Error> {
        let decoder = note_id.decoder();
        decode!(decoder, note_id.id)
    }
}

impl From<&NoteId> for proto::note::NoteId {
    fn from(note_id: &NoteId) -> Self {
        Self { id: Some(note_id.into()) }
    }
}

impl From<(&NoteId, &NoteInclusionProof)> for proto::note::NoteInclusionInBlockProof {
    fn from((note_id, proof): (&NoteId, &NoteInclusionProof)) -> Self {
        Self {
            note_id: Some(note_id.into()),
            block_num: proof.location().block_num().as_u32(),
            note_index_in_block: proof.location().block_note_tree_index().into(),
            inclusion_path: Some(proof.note_path().clone().into()),
        }
    }
}

impl TryFrom<&proto::note::NoteInclusionInBlockProof> for (NoteId, NoteInclusionProof) {
    type Error = ConversionError;

    fn try_from(
        proof: &proto::note::NoteInclusionInBlockProof,
    ) -> Result<(NoteId, NoteInclusionProof), Self::Error> {
        let decoder = proof.decoder();
        let inclusion_path: SparseMerklePath =
            decoder.decode_field("inclusion_path", proof.inclusion_path.clone())?;
        let note_id: Word = decode!(decoder, proof.note_id)?;

        Ok((
            NoteId::from_raw(note_id),
            NoteInclusionProof::new(
                proof.block_num.into(),
                proof.note_index_in_block.try_into().context("note_index_in_block")?,
                inclusion_path,
            )?,
        ))
    }
}

// NOTE HEADER
// ================================================================================================

impl From<NoteHeader> for proto::note::NoteHeader {
    fn from(header: NoteHeader) -> Self {
        Self {
            details_commitment: Some(header.details_commitment().as_word().into()),
            metadata: Some(header.into_metadata().into()),
        }
    }
}

impl TryFrom<proto::note::NoteHeader> for NoteHeader {
    type Error = ConversionError;

    fn try_from(value: proto::note::NoteHeader) -> Result<Self, Self::Error> {
        let decoder = value.decoder();
        let details_commitment_word: Word = decode!(decoder, value.details_commitment)?;
        let metadata: NoteMetadata = decode!(decoder, value.metadata)?;

        Ok(NoteHeader::new(
            NoteDetailsCommitment::from_raw(details_commitment_word),
            metadata,
        ))
    }
}

// NOTE SCRIPT
// ================================================================================================

impl From<NoteScript> for proto::note::NoteScript {
    fn from(script: NoteScript) -> Self {
        Self {
            entrypoint: script.entrypoint().into(),
            mast: script.mast().to_bytes(),
        }
    }
}

impl TryFrom<proto::note::NoteScript> for NoteScript {
    type Error = ConversionError;

    fn try_from(value: proto::note::NoteScript) -> Result<Self, Self::Error> {
        let proto::note::NoteScript { entrypoint, mast } = value;

        let mast = MastForest::decode_bytes(&mast, "note_script.mast")?;
        let entrypoint = MastNodeId::from_u32_safe(entrypoint, &mast)
            .map_err(|err| ConversionError::deserialization("note_script.entrypoint", err))?;

        Ok(Self::from_parts(Arc::new(mast), entrypoint))
    }
}

// HELPERS
// ================================================================================================

/// Decodes the `(sender, note_type, tag)` triple from a proto `NoteMetadata` into a
/// [`PartialNoteMetadata`]. The attachment-related fields on the proto are ignored — when full
/// attachments are also transmitted, the receiver derives the canonical headers and commitment from
/// those instead.
fn partial_note_metadata_from_proto(
    value: proto::note::NoteMetadata,
) -> Result<PartialNoteMetadata, ConversionError> {
    let decoder = value.decoder();
    let sender = decode!(decoder, value.sender)?;
    let note_type = proto::note::NoteType::try_from(value.note_type)
        .map_err(|_| ConversionError::message("enum variant discriminant out of range"))?
        .try_into()
        .context("note_type")?;
    let tag = NoteTag::new(value.tag);
    Ok(PartialNoteMetadata::new(sender, note_type).with_tag(tag))
}

/// Requires and decodes the structured attachments carried by a note message.
fn decode_note_attachments<M: prost::Message>(
    attachments: Option<proto::note::NoteAttachments>,
) -> Result<NoteAttachments, ConversionError> {
    GrpcStructDecoder::<M>::default().decode_field("note_attachments", attachments)
}

#[cfg(test)]
mod tests {
    use miden_protocol::account::{AccountId, AccountIdVersion, AccountType, AssetCallbackFlag};

    use super::*;

    fn word(value: u32) -> Word {
        Word::from([value, value + 1, value + 2, value + 3])
    }

    fn attachment(scheme: u16, num_words: usize, first_word: u32) -> NoteAttachment {
        let words = (0..num_words)
            .map(|index| word(first_word + u32::try_from(index).unwrap() * 4))
            .collect();
        NoteAttachment::with_words(NoteAttachmentScheme::new(scheme).unwrap(), words).unwrap()
    }

    fn proto_attachment(scheme: u32, num_words: usize) -> proto::note::NoteAttachment {
        proto::note::NoteAttachment {
            scheme,
            words: vec![
                proto::primitives::Word { encoded: vec![0; Word::SERIALIZED_SIZE] };
                num_words
            ],
        }
    }

    fn note_with_attachments(attachments: NoteAttachments) -> Note {
        let base = Note::mock_noop(word(100));
        let (assets, metadata, recipient, _) = base.into_parts();
        Note::with_attachments(assets, metadata.into_partial_metadata(), recipient, attachments)
    }

    #[test]
    fn note_header_roundtrip_preserves_id() {
        // Build a NoteHeader with a known details_commitment and metadata.
        let details_commitment =
            NoteDetailsCommitment::from_raw(Word::try_from([1u64, 2, 3, 4]).unwrap());
        let sender = AccountId::dummy(
            [1; 15],
            AccountIdVersion::Version1,
            AccountType::Public,
            AssetCallbackFlag::Disabled,
        );
        let metadata = NoteMetadata::new(
            PartialNoteMetadata::new(sender, NoteType::Public).with_tag(NoteTag::from(7u32)),
            &NoteAttachments::default(),
        );

        let original = NoteHeader::new(details_commitment, metadata);

        // Round-trip through proto.
        let proto_header: proto::note::NoteHeader = original.into();
        let decoded = NoteHeader::try_from(proto_header).expect("proto NoteHeader should decode");

        // Both the derived id and the details_commitment must match — guards against the historical
        // bug where the encoder wrote `id` into the same wire field the decoder interpreted as
        // `details_commitment`.
        assert_eq!(decoded.id(), original.id());
        assert_eq!(decoded.details_commitment(), original.details_commitment());
        assert_eq!(decoded.metadata(), original.metadata());
    }

    #[test]
    fn empty_attachments_roundtrip() {
        let original = NoteAttachments::empty();
        let encoded = proto::note::NoteAttachments::from(original.clone());

        assert!(encoded.attachments.is_empty());
        assert_eq!(NoteAttachments::try_from(encoded).unwrap(), original);
    }

    #[test]
    fn one_attachment_with_none_scheme_roundtrips() {
        let original =
            NoteAttachments::from(NoteAttachment::with_word(NoteAttachmentScheme::none(), word(1)));
        let encoded = proto::note::NoteAttachments::from(&original);

        assert_eq!(encoded.attachments[0].scheme, 1);
        assert_eq!(NoteAttachments::try_from(encoded).unwrap(), original);
    }

    #[test]
    fn attachment_and_word_order_and_duplicate_schemes_are_preserved() {
        let original = NoteAttachments::new(vec![
            attachment(42, 3, 1),
            attachment(42, 2, 101),
            attachment(7, 1, 201),
        ])
        .unwrap();

        let encoded = proto::note::NoteAttachments::from(&original);
        assert_eq!(
            encoded.attachments.iter().map(|item| item.scheme).collect::<Vec<_>>(),
            [42, 42, 7]
        );
        assert_eq!(
            encoded.attachments[0]
                .words
                .iter()
                .map(|item| Word::try_from(item).unwrap())
                .collect::<Vec<_>>(),
            original.get(0).unwrap().content().as_words()
        );
        assert_eq!(NoteAttachments::try_from(encoded).unwrap(), original);
    }

    #[test]
    fn attachment_boundaries_are_accepted() {
        let four = NoteAttachments::new(vec![
            attachment(1, 1, 1),
            attachment(2, 1, 10),
            attachment(3, 1, 20),
            attachment(4, 1, 30),
        ])
        .unwrap();
        assert_eq!(
            NoteAttachments::try_from(proto::note::NoteAttachments::from(four.clone())).unwrap(),
            four
        );

        let max_single = NoteAttachments::from(attachment(1, 256, 1));
        assert_eq!(
            NoteAttachments::try_from(proto::note::NoteAttachments::from(max_single.clone()))
                .unwrap(),
            max_single
        );

        let max_total = NoteAttachments::new(vec![
            attachment(1, 128, 1),
            attachment(2, 128, 1001),
            attachment(3, 128, 2001),
            attachment(4, 128, 3001),
        ])
        .unwrap();
        assert_eq!(
            NoteAttachments::try_from(proto::note::NoteAttachments::from(max_total.clone()))
                .unwrap(),
            max_total
        );
    }

    #[test]
    fn invalid_attachment_schemes_are_rejected() {
        for scheme in [0, 65_535, u32::from(u16::MAX) + 1] {
            let err = NoteAttachment::try_from(proto_attachment(scheme, 1)).unwrap_err();
            assert!(err.to_string().starts_with("scheme:"), "unexpected error: {err}");
        }
    }

    #[test]
    fn invalid_attachment_sizes_are_rejected() {
        let empty = NoteAttachment::try_from(proto_attachment(1, 0)).unwrap_err();
        assert!(empty.to_string().starts_with("words:"), "unexpected error: {empty}");

        let too_large = NoteAttachment::try_from(proto_attachment(1, 257)).unwrap_err();
        assert!(too_large.to_string().starts_with("words:"), "unexpected error: {too_large}");
    }

    #[test]
    fn invalid_attachment_collections_are_rejected() {
        let five = proto::note::NoteAttachments {
            attachments: (1..=5).map(|scheme| proto_attachment(scheme, 1)).collect(),
        };
        let err = NoteAttachments::try_from(five).unwrap_err();
        assert!(err.to_string().starts_with("attachments:"), "unexpected error: {err}");

        let over_total = proto::note::NoteAttachments {
            attachments: vec![
                proto_attachment(1, 171),
                proto_attachment(2, 171),
                proto_attachment(3, 171),
            ],
        };
        let err = NoteAttachments::try_from(over_total).unwrap_err();
        assert!(err.to_string().starts_with("attachments:"), "unexpected error: {err}");
    }

    #[test]
    fn malformed_primitive_word_is_rejected_with_context() {
        let value = proto::note::NoteAttachment {
            scheme: 1,
            words: vec![proto::primitives::Word { encoded: vec![0; 31] }],
        };
        let err = NoteAttachment::try_from(value).unwrap_err();

        assert!(err.to_string().starts_with("words.word.encoded:"), "unexpected error: {err}");
    }

    #[test]
    fn attachment_commitment_roundtrips() {
        let original =
            NoteAttachments::new(vec![attachment(11, 4, 1), attachment(12, 3, 100)]).unwrap();
        let expected_commitment = original.to_commitment();
        let decoded =
            NoteAttachments::try_from(proto::note::NoteAttachments::from(original)).unwrap();

        assert_eq!(decoded.to_commitment(), expected_commitment);
    }

    #[test]
    fn note_encoding_keeps_attachments_and_metadata_consistent() {
        let attachments =
            NoteAttachments::new(vec![attachment(11, 2, 1), attachment(11, 3, 100)]).unwrap();
        let note = note_with_attachments(attachments.clone());
        let encoded = proto::note::Note::from(note.clone());
        let metadata = encoded.metadata.as_ref().unwrap();
        let encoded_attachments = encoded.note_attachments.as_ref().unwrap();

        assert_eq!(
            metadata.attachment_schemes,
            attachments
                .to_headers()
                .iter()
                .map(|header| u32::from(header.scheme().map_or(0, |scheme| scheme.as_u16())))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            Word::try_from(metadata.attachments_commitment.as_ref().unwrap()).unwrap(),
            attachments.to_commitment()
        );
        assert_eq!(NoteAttachments::try_from(encoded_attachments.clone()).unwrap(), attachments);

        let decoded = Note::try_from(encoded).unwrap();
        assert_eq!(decoded.attachments(), note.attachments());
        assert_eq!(
            decoded.metadata().attachments_commitment(),
            note.metadata().attachments_commitment()
        );
    }

    #[test]
    fn missing_structured_attachments_are_rejected() {
        let mut encoded = proto::note::Note::from(note_with_attachments(NoteAttachments::empty()));
        encoded.note_attachments = None;

        let err = Note::try_from(encoded).unwrap_err();
        assert!(err.to_string().contains("note_attachments"), "unexpected error: {err}");

        let default_note = proto::note::Note::default();
        let err = decode_note_attachments::<proto::note::Note>(default_note.note_attachments)
            .unwrap_err();
        assert!(err.to_string().contains("note_attachments"), "unexpected error: {err}");

        let default_network_note = proto::note::NetworkNote::default();
        let err = decode_note_attachments::<proto::note::NetworkNote>(
            default_network_note.note_attachments,
        )
        .unwrap_err();
        assert!(err.to_string().contains("note_attachments"), "unexpected error: {err}");
    }
}
