use std::collections::BTreeMap;
use std::sync::Arc;

use miden_protocol::Word;
use miden_protocol::account::{AccountId, AccountUpdateDetails};
use miden_protocol::batch::{BatchAccountUpdate, ProposedBatch, ProvenBatch};
use miden_protocol::block::{BlockHeader, BlockNumber};
use miden_protocol::note::{NoteId, NoteInclusionProof};
use miden_protocol::transaction::{
    InputNoteCommitment,
    OrderedTransactionHeaders,
    OutputNote,
    PartialBlockchain,
    ProvenTransaction,
    TransactionHeader,
};

use crate::decode::{ConversionResultExt, GrpcDecodeExt};
use crate::errors::ConversionError;
use crate::{decode, generated as proto};

/// Data required for a transaction batch.
#[derive(Clone, Debug)]
pub struct BatchInputs {
    pub batch_reference_block_header: BlockHeader,
    pub note_proofs: BTreeMap<NoteId, NoteInclusionProof>,
    pub partial_block_chain: PartialBlockchain,
}

impl From<&BatchAccountUpdate> for proto::transaction::BatchAccountUpdate {
    fn from(value: &BatchAccountUpdate) -> Self {
        Self {
            account_id: Some(value.account_id().into()),
            initial_state_commitment: Some(value.initial_state_commitment().into()),
            final_state_commitment: Some(value.final_state_commitment().into()),
            details: Some(value.details().into()),
        }
    }
}

impl From<&ProposedBatch> for proto::transaction::ProposedBatch {
    fn from(value: &ProposedBatch) -> Self {
        let (
            transactions,
            reference_block_header,
            partial_blockchain,
            unauthenticated_note_proofs,
            ..,
        ) = value.clone().into_parts();

        Self {
            transactions: transactions.iter().map(|tx| tx.as_ref().into()).collect(),
            reference_block_header: Some(reference_block_header.into()),
            partial_blockchain: Some((&partial_blockchain).into()),
            unauthenticated_note_proofs: unauthenticated_note_proofs
                .iter()
                .map(Into::into)
                .collect(),
        }
    }
}

impl From<ProposedBatch> for proto::transaction::ProposedBatch {
    fn from(value: ProposedBatch) -> Self {
        Self::from(&value)
    }
}

/// Decodes and structurally validates a proposed batch, including transaction proof verification.
///
/// Callers handling untrusted requests should invoke this inside a blocking task.
pub fn decode_proposed_batch(
    value: proto::transaction::ProposedBatch,
    proof_security_level: u32,
) -> Result<ProposedBatch, ConversionError> {
    let decoder = value.decoder();
    let transactions = value
        .transactions
        .into_iter()
        .enumerate()
        .map(|(index, tx)| {
            ProvenTransaction::try_from(tx)
                .map(Arc::new)
                .context(format!("transactions[{index}]"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let reference_block_header = decode!(decoder, value.reference_block_header)?;
    let partial_blockchain = decode!(decoder, value.partial_blockchain)?;

    let mut unauthenticated_note_proofs = BTreeMap::new();
    let mut previous_note_id = None;
    for (index, proof) in value.unauthenticated_note_proofs.iter().enumerate() {
        let (note_id, proof) = <(NoteId, NoteInclusionProof)>::try_from(proof)
            .context(format!("unauthenticated_note_proofs[{index}]"))?;
        if previous_note_id.is_some_and(|previous| note_id <= previous) {
            return Err(ConversionError::message(
                "unauthenticated note proofs must have unique, ascending note IDs",
            )
            .context(format!("unauthenticated_note_proofs[{index}].note_id")));
        }
        previous_note_id = Some(note_id);
        unauthenticated_note_proofs.insert(note_id, proof);
    }

    ProposedBatch::new(
        transactions,
        reference_block_header,
        partial_blockchain,
        unauthenticated_note_proofs,
        proof_security_level,
    )
    .map_err(ConversionError::new)
}

impl From<&ProvenBatch> for proto::transaction::ProvenBatch {
    fn from(value: &ProvenBatch) -> Self {
        Self {
            reference_block_commitment: Some(value.reference_block_commitment().into()),
            reference_block_num: value.reference_block_num().as_u32(),
            account_updates: value.account_updates().values().map(Into::into).collect(),
            input_notes: value.input_notes().iter().map(Into::into).collect(),
            output_notes: value.output_notes().iter().map(Into::into).collect(),
            expiration_block_num: value.batch_expiration_block_num().as_u32(),
            transactions: value.transactions().as_slice().iter().map(Into::into).collect(),
            proof: Some(value.proof().into()),
        }
    }
}

impl From<ProvenBatch> for proto::transaction::ProvenBatch {
    fn from(value: ProvenBatch) -> Self {
        Self::from(&value)
    }
}

#[derive(PartialEq, Eq)]
struct BatchAccountUpdateProjection {
    account_id: AccountId,
    initial_state_commitment: Word,
    final_state_commitment: Word,
    details: AccountUpdateDetails,
}

impl TryFrom<proto::transaction::BatchAccountUpdate> for BatchAccountUpdateProjection {
    type Error = ConversionError;

    fn try_from(value: proto::transaction::BatchAccountUpdate) -> Result<Self, Self::Error> {
        let decoder = value.decoder();
        Ok(Self {
            account_id: decode!(decoder, value.account_id)?,
            initial_state_commitment: decode!(decoder, value.initial_state_commitment)?,
            final_state_commitment: decode!(decoder, value.final_state_commitment)?,
            details: decode!(decoder, value.details)?,
        })
    }
}

/// Decodes a proven batch and checks every duplicated public field against its proposal.
pub fn decode_proven_batch(
    value: proto::transaction::ProvenBatch,
    proposed: &ProposedBatch,
) -> Result<ProvenBatch, ConversionError> {
    let decoder = value.decoder();
    let reference_block_commitment: Word = decode!(decoder, value.reference_block_commitment)?;
    let reference_block_num = BlockNumber::from(value.reference_block_num);
    let expected_header = proposed.reference_block_header();
    if reference_block_num != expected_header.block_num() {
        return Err(ConversionError::message("reference block number does not match proposal")
            .context("reference_block_num"));
    }
    if reference_block_commitment != expected_header.commitment() {
        return Err(ConversionError::message("reference block commitment does not match proposal")
            .context("reference_block_commitment"));
    }

    if value.account_updates.len() != proposed.account_updates().len() {
        return Err(ConversionError::message("account updates do not match proposal")
            .context("account_updates"));
    }
    let mut previous_account_id = None;
    for (index, update) in value.account_updates.into_iter().enumerate() {
        let update = BatchAccountUpdateProjection::try_from(update)
            .context(format!("account_updates[{index}]"))?;
        if previous_account_id.is_some_and(|previous| update.account_id <= previous) {
            return Err(ConversionError::message(
                "account updates must have unique, ascending account IDs",
            )
            .context(format!("account_updates[{index}].account_id")));
        }
        previous_account_id = Some(update.account_id);
        let expected = proposed.account_updates().get(&update.account_id).ok_or_else(|| {
            ConversionError::message("account update is absent from proposal")
                .context(format!("account_updates[{index}].account_id"))
        })?;
        let expected = BatchAccountUpdateProjection {
            account_id: expected.account_id(),
            initial_state_commitment: expected.initial_state_commitment(),
            final_state_commitment: expected.final_state_commitment(),
            details: expected.details().clone(),
        };
        if update != expected {
            return Err(ConversionError::message("account update does not match proposal")
                .context(format!("account_updates[{index}]")));
        }
    }

    let input_notes = value
        .input_notes
        .into_iter()
        .enumerate()
        .map(|(index, note)| {
            InputNoteCommitment::try_from(note).context(format!("input_notes[{index}]"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if !input_notes.iter().eq(proposed.input_notes().iter()) {
        return Err(
            ConversionError::message("input notes do not match proposal").context("input_notes")
        );
    }

    let output_notes = value
        .output_notes
        .into_iter()
        .enumerate()
        .map(|(index, note)| OutputNote::try_from(note).context(format!("output_notes[{index}]")))
        .collect::<Result<Vec<_>, _>>()?;
    if output_notes != proposed.output_notes() {
        return Err(
            ConversionError::message("output notes do not match proposal").context("output_notes")
        );
    }

    let expiration = BlockNumber::from(value.expiration_block_num);
    if expiration != proposed.batch_expiration_block_num() {
        return Err(ConversionError::message("expiration block does not match proposal")
            .context("expiration_block_num"));
    }

    let transactions = value
        .transactions
        .into_iter()
        .enumerate()
        .map(|(index, tx)| {
            TransactionHeader::try_from(tx).context(format!("transactions[{index}]"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let expected_transactions = proposed.transaction_headers();
    if transactions.as_slice() != expected_transactions.as_slice() {
        return Err(ConversionError::message("transaction headers do not match proposal")
            .context("transactions"));
    }

    let proof = decode!(decoder, value.proof)?;
    ProvenBatch::new_unchecked(
        proposed.id(),
        expected_header.commitment(),
        expected_header.block_num(),
        proposed.account_updates().clone(),
        proposed.input_notes().clone(),
        proposed.output_notes().to_vec(),
        proposed.batch_expiration_block_num(),
        OrderedTransactionHeaders::new_unchecked(transactions),
        proof,
    )
    .map_err(ConversionError::new)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use miden_protocol::Word;
    use miden_protocol::account::{
        AccountId,
        AccountIdVersion,
        AccountType,
        AccountUpdateDetails,
        AssetCallbackFlag,
    };
    use miden_protocol::batch::{ProposedBatch, ProvenBatch};
    use miden_protocol::block::BlockHeader;
    use miden_protocol::transaction::{
        InputNoteCommitment,
        OutputNote,
        PartialBlockchain,
        ProvenTransaction,
        TxAccountUpdate,
    };
    use miden_protocol::vm::ExecutionProof;

    use super::decode_proven_batch;
    use crate::generated as proto;

    fn proposal_and_proof() -> (ProposedBatch, ProvenBatch) {
        let partial_blockchain = PartialBlockchain::default();
        let reference_block_header = BlockHeader::mock(
            0,
            Some(partial_blockchain.peaks().hash_peaks()),
            None,
            &[],
            Word::default(),
        );
        let account_id = AccountId::dummy(
            [8; 15],
            AccountIdVersion::Version1,
            AccountType::Private,
            AssetCallbackFlag::Disabled,
        );
        let account_update = TxAccountUpdate::new(
            account_id,
            Word::from([1_u32, 2, 3, 4]),
            Word::from([5_u32, 6, 7, 8]),
            Word::from([9_u32, 10, 11, 12]),
            AccountUpdateDetails::Private,
        )
        .unwrap();
        let transaction = ProvenTransaction::new(
            account_update,
            Vec::<InputNoteCommitment>::new(),
            Vec::<OutputNote>::new(),
            reference_block_header.block_num(),
            reference_block_header.commitment(),
            reference_block_header.block_num() + 1,
            ExecutionProof::new_dummy(),
        )
        .unwrap();
        let proposed = ProposedBatch::new_unverified(
            vec![Arc::new(transaction)],
            reference_block_header,
            partial_blockchain,
            BTreeMap::new(),
        )
        .unwrap();
        let proven = ProvenBatch::new_unchecked(
            proposed.id(),
            proposed.reference_block_header().commitment(),
            proposed.reference_block_header().block_num(),
            proposed.account_updates().clone(),
            proposed.input_notes().clone(),
            proposed.output_notes().to_vec(),
            proposed.batch_expiration_block_num(),
            proposed.transaction_headers(),
            ExecutionProof::new_dummy(),
        )
        .unwrap();
        (proposed, proven)
    }

    #[test]
    fn proven_batch_roundtrips_only_with_its_proposal() {
        let (proposed, proven) = proposal_and_proof();
        let encoded = proto::transaction::ProvenBatch::from(&proven);
        assert_eq!(decode_proven_batch(encoded.clone(), &proposed).unwrap(), proven);

        let mut wrong_reference = encoded.clone();
        wrong_reference.reference_block_num += 1;
        assert!(
            decode_proven_batch(wrong_reference, &proposed)
                .unwrap_err()
                .to_string()
                .contains("reference_block_num")
        );

        let mut duplicate_account = encoded;
        duplicate_account
            .account_updates
            .push(duplicate_account.account_updates[0].clone());
        assert!(
            decode_proven_batch(duplicate_account, &proposed)
                .unwrap_err()
                .to_string()
                .contains("account_updates")
        );
    }
}
