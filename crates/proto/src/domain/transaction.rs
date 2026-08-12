use miden_protocol::Word;
use miden_protocol::account::AccountId;
use miden_protocol::note::{Note, NoteHeader, Nullifier};
use miden_protocol::transaction::{
    InputNoteCommitment,
    InputNotes,
    OutputNote,
    PrivateOutputNote,
    PublicOutputNote,
    TransactionHeader,
    TransactionId,
};

use crate::decode::{ConversionResultExt, GrpcDecodeExt};
use crate::errors::ConversionError;
use crate::{decode, generated as proto};

// FROM TRANSACTION ID
// ================================================================================================

impl From<&TransactionId> for proto::primitives::Digest {
    fn from(value: &TransactionId) -> Self {
        value.as_word().into()
    }
}

impl From<TransactionId> for proto::primitives::Digest {
    fn from(value: TransactionId) -> Self {
        value.as_word().into()
    }
}

impl From<&TransactionId> for proto::transaction::TransactionId {
    fn from(value: &TransactionId) -> Self {
        proto::transaction::TransactionId { id: Some(value.into()) }
    }
}

impl From<TransactionId> for proto::transaction::TransactionId {
    fn from(value: TransactionId) -> Self {
        (&value).into()
    }
}

// INTO TRANSACTION ID
// ================================================================================================

impl TryFrom<proto::primitives::Digest> for TransactionId {
    type Error = ConversionError;

    fn try_from(value: proto::primitives::Digest) -> Result<Self, Self::Error> {
        let digest: Word = value.try_into()?;
        Ok(TransactionId::from_raw(digest))
    }
}

impl TryFrom<proto::transaction::TransactionId> for TransactionId {
    type Error = ConversionError;

    fn try_from(value: proto::transaction::TransactionId) -> Result<Self, Self::Error> {
        let decoder = value.decoder();
        decode!(decoder, value.id)
    }
}

// INPUT NOTE COMMITMENT
// ================================================================================================

impl From<InputNoteCommitment> for proto::transaction::InputNoteCommitment {
    fn from(value: InputNoteCommitment) -> Self {
        Self::from(&value)
    }
}

impl From<&InputNoteCommitment> for proto::transaction::InputNoteCommitment {
    fn from(value: &InputNoteCommitment) -> Self {
        Self {
            nullifier: Some(value.nullifier().into()),
            header: value.header().copied().map(Into::into),
        }
    }
}

impl TryFrom<proto::transaction::InputNoteCommitment> for InputNoteCommitment {
    type Error = ConversionError;

    fn try_from(value: proto::transaction::InputNoteCommitment) -> Result<Self, Self::Error> {
        let decoder = value.decoder();
        let nullifier: Nullifier = decode!(decoder, value.nullifier)?;

        let header: Option<miden_protocol::note::NoteHeader> =
            value.header.map(TryInto::try_into).transpose().context("header")?;

        Ok(InputNoteCommitment::from_parts_unchecked(nullifier, header))
    }
}

// TRANSACTION HEADER
// ================================================================================================

impl From<&TransactionHeader> for proto::transaction::TransactionHeader {
    fn from(header: &TransactionHeader) -> Self {
        Self {
            transaction_id: Some(header.id().into()),
            account_id: Some(header.account_id().into()),
            initial_state_commitment: Some(header.initial_state_commitment().into()),
            final_state_commitment: Some(header.final_state_commitment().into()),
            input_notes: header.input_notes().iter().map(Into::into).collect(),
            output_notes: header.output_notes().iter().copied().map(Into::into).collect(),
        }
    }
}

impl From<TransactionHeader> for proto::transaction::TransactionHeader {
    fn from(header: TransactionHeader) -> Self {
        Self::from(&header)
    }
}

impl TryFrom<proto::transaction::TransactionHeader> for TransactionHeader {
    type Error = ConversionError;

    fn try_from(header: proto::transaction::TransactionHeader) -> Result<Self, Self::Error> {
        let decoder = header.decoder();
        let transmitted_id: TransactionId = decode!(decoder, header.transaction_id)?;
        let account_id: AccountId = decode!(decoder, header.account_id)?;
        let initial_state_commitment = decode!(decoder, header.initial_state_commitment)?;
        let final_state_commitment = decode!(decoder, header.final_state_commitment)?;
        let input_notes = header
            .input_notes
            .into_iter()
            .enumerate()
            .map(|(index, note)| {
                InputNoteCommitment::try_from(note).context(format!("input_notes[{index}]"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let input_notes = InputNotes::new(input_notes)
            .map_err(ConversionError::new)
            .context("input_notes")?;
        let output_notes = header
            .output_notes
            .into_iter()
            .enumerate()
            .map(|(index, note)| {
                NoteHeader::try_from(note).context(format!("output_notes[{index}]"))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let header = TransactionHeader::new(
            account_id,
            initial_state_commitment,
            final_state_commitment,
            input_notes,
            output_notes,
        );
        if header.id() != transmitted_id {
            return Err(ConversionError::message(format!(
                "transaction ID mismatch: transmitted {transmitted_id}, recomputed {}",
                header.id()
            ))
            .context("transaction_id"));
        }

        Ok(header)
    }
}

// OUTPUT NOTES
// ================================================================================================

impl From<&PublicOutputNote> for proto::transaction::PublicOutputNote {
    fn from(note: &PublicOutputNote) -> Self {
        let details = proto::note::NoteDetails {
            assets: note.assets().iter().copied().map(Into::into).collect(),
            recipient: Some(note.recipient().into()),
        };
        Self {
            metadata: Some((*note.metadata()).into()),
            details: Some(details),
            attachments: Some(note.as_note().attachments().into()),
        }
    }
}

impl From<PublicOutputNote> for proto::transaction::PublicOutputNote {
    fn from(note: PublicOutputNote) -> Self {
        Self::from(&note)
    }
}

impl TryFrom<proto::transaction::PublicOutputNote> for PublicOutputNote {
    type Error = ConversionError;

    fn try_from(note: proto::transaction::PublicOutputNote) -> Result<Self, Self::Error> {
        let domain_note = Note::try_from(proto::note::Note {
            metadata: note.metadata,
            note_details: note.details,
            note_attachments: note.attachments,
        })?;
        PublicOutputNote::new(domain_note).map_err(ConversionError::new)
    }
}

impl From<&PrivateOutputNote> for proto::transaction::PrivateOutputNote {
    fn from(note: &PrivateOutputNote) -> Self {
        Self {
            header: Some((*note.header()).into()),
            attachments: Some(note.attachments().into()),
        }
    }
}

impl From<PrivateOutputNote> for proto::transaction::PrivateOutputNote {
    fn from(note: PrivateOutputNote) -> Self {
        Self::from(&note)
    }
}

impl TryFrom<proto::transaction::PrivateOutputNote> for PrivateOutputNote {
    type Error = ConversionError;

    fn try_from(note: proto::transaction::PrivateOutputNote) -> Result<Self, Self::Error> {
        let decoder = note.decoder();
        let header = decode!(decoder, note.header)?;
        let attachments = decode!(decoder, note.attachments)?;
        PrivateOutputNote::new(header, attachments).map_err(ConversionError::new)
    }
}

impl From<&OutputNote> for proto::transaction::OutputNote {
    fn from(note: &OutputNote) -> Self {
        use proto::transaction::output_note::Note;

        let note = match note {
            OutputNote::Public(note) => Note::Public(note.into()),
            OutputNote::Private(note) => Note::Private(note.into()),
        };
        Self { note: Some(note) }
    }
}

impl From<OutputNote> for proto::transaction::OutputNote {
    fn from(note: OutputNote) -> Self {
        Self::from(&note)
    }
}

impl TryFrom<proto::transaction::OutputNote> for OutputNote {
    type Error = ConversionError;

    fn try_from(note: proto::transaction::OutputNote) -> Result<Self, Self::Error> {
        use proto::transaction::output_note::Note;

        match note.note {
            Some(Note::Public(note)) => note.try_into().map(OutputNote::Public).context("public"),
            Some(Note::Private(note)) => {
                note.try_into().map(OutputNote::Private).context("private")
            },
            None => Err(ConversionError::missing_field::<proto::transaction::OutputNote>("note")),
        }
    }
}

#[cfg(test)]
mod tests {
    use miden_protocol::Word;
    use miden_protocol::account::{AccountId, AccountIdVersion, AccountType, AssetCallbackFlag};
    use miden_protocol::note::{Note, NoteAttachments};
    use miden_protocol::transaction::{
        InputNoteCommitment,
        InputNotes,
        OutputNote,
        PrivateOutputNote,
        TransactionHeader,
    };

    use crate::generated as proto;

    fn account_id() -> AccountId {
        AccountId::dummy(
            [9; 15],
            AccountIdVersion::Version1,
            AccountType::Private,
            AssetCallbackFlag::Disabled,
        )
    }

    #[test]
    fn transaction_header_roundtrips_and_rejects_a_mismatched_id() {
        let header = TransactionHeader::new(
            account_id(),
            Word::from([1_u32, 2, 3, 4]),
            Word::from([5_u32, 6, 7, 8]),
            InputNotes::<InputNoteCommitment>::new(Vec::new()).unwrap(),
            Vec::new(),
        );
        let encoded = proto::transaction::TransactionHeader::from(&header);
        assert_eq!(TransactionHeader::try_from(encoded.clone()).unwrap(), header);

        let mut mismatched = encoded;
        mismatched.transaction_id =
            Some(miden_protocol::transaction::TransactionId::from_raw(Word::default()).into());
        let error = TransactionHeader::try_from(mismatched).unwrap_err().to_string();
        assert!(error.contains("transaction_id"));
    }

    #[test]
    fn private_output_note_roundtrips_and_oneof_is_required() {
        let note = Note::mock_noop(Word::from([4_u32, 3, 2, 1]));
        let output = OutputNote::Private(
            PrivateOutputNote::new(*note.header(), NoteAttachments::default()).unwrap(),
        );
        assert_eq!(
            OutputNote::try_from(proto::transaction::OutputNote::from(&output)).unwrap(),
            output
        );

        let error = OutputNote::try_from(proto::transaction::OutputNote::default())
            .unwrap_err()
            .to_string();
        assert!(error.contains("note"));
    }
}
