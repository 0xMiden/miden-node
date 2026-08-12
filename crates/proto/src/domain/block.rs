use std::collections::BTreeSet;
use std::ops::RangeInclusive;

use miden_protocol::account::{AccountId, AccountUpdateDetails};
use miden_protocol::block::{
    BlockAccountUpdate,
    BlockBody,
    BlockHeader,
    BlockNoteIndex,
    BlockNumber,
    BlockProof,
    BlockSignatures,
    FeeParameters,
    OutputNoteBatch,
    SignedBlock,
    ValidatorKeys,
};
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::{PublicKey, Signature};
use miden_protocol::crypto::merkle::MerklePath;
use miden_protocol::crypto::merkle::mmr::{Forest, MmrPeaks, PartialMmr};
use miden_protocol::note::Nullifier;
use miden_protocol::transaction::{
    OrderedTransactionHeaders,
    OutputNote,
    PartialBlockchain,
    TransactionHeader,
};
use miden_protocol::utils::serde::{Deserializable, Serializable};
use miden_protocol::{MAX_BATCHES_PER_BLOCK, MAX_OUTPUT_NOTES_PER_BATCH, Word};
use thiserror::Error;

use crate::decode::{ConversionResultExt, DecodeBytesExt, GrpcDecodeExt};
use crate::errors::ConversionError;
use crate::{decode, generated as proto};

// BLOCK NUMBER
// ================================================================================================

impl From<BlockNumber> for proto::blockchain::BlockNumber {
    fn from(value: BlockNumber) -> Self {
        proto::blockchain::BlockNumber { block_num: value.as_u32() }
    }
}

impl From<proto::blockchain::BlockNumber> for BlockNumber {
    fn from(value: proto::blockchain::BlockNumber) -> Self {
        BlockNumber::from(value.block_num)
    }
}

// PARTIAL BLOCKCHAIN
// ================================================================================================

impl From<&PartialBlockchain> for proto::blockchain::PartialBlockchain {
    fn from(value: &PartialBlockchain) -> Self {
        let mmr = value.mmr();
        let tracked_leaves = mmr
            .leaves()
            .map(|(position, leaf)| {
                let proof = mmr
                    .open(position)
                    .expect("tracked MMR position must be in bounds")
                    .expect("tracked MMR leaf must have an opening");
                proto::blockchain::TrackedMmrLeaf {
                    position: position as u64,
                    leaf: Some(leaf.into()),
                    path: proof.merkle_path().nodes().iter().map(Into::into).collect(),
                }
            })
            .collect();
        let peaks = mmr.peaks();
        Self {
            forest: mmr.forest().num_leaves() as u64,
            peaks: peaks.peaks().iter().map(Into::into).collect(),
            tracked_leaves,
            block_headers: value.block_headers().map(Into::into).collect(),
        }
    }
}

impl TryFrom<proto::blockchain::PartialBlockchain> for PartialBlockchain {
    type Error = ConversionError;

    fn try_from(value: proto::blockchain::PartialBlockchain) -> Result<Self, Self::Error> {
        let forest_size =
            usize::try_from(value.forest).map_err(ConversionError::new).context("forest")?;
        let forest = Forest::new(forest_size).map_err(ConversionError::new).context("forest")?;
        let peaks = value
            .peaks
            .into_iter()
            .enumerate()
            .map(|(index, peak)| Word::try_from(peak).context(format!("peaks[{index}]")))
            .collect::<Result<Vec<_>, _>>()?;
        let peaks = MmrPeaks::new(forest, peaks).map_err(ConversionError::new).context("peaks")?;
        let mut mmr = PartialMmr::from_peaks(peaks);

        let mut previous_position = None;
        for (index, tracked) in value.tracked_leaves.into_iter().enumerate() {
            let position = usize::try_from(tracked.position)
                .map_err(ConversionError::new)
                .context(format!("tracked_leaves[{index}].position"))?;
            if position >= forest_size {
                return Err(ConversionError::message(format!(
                    "tracked leaf position {position} is outside forest of size {forest_size}"
                ))
                .context(format!("tracked_leaves[{index}].position")));
            }
            if previous_position.is_some_and(|previous| position <= previous) {
                return Err(ConversionError::message(
                    "tracked leaf positions must be unique and strictly increasing",
                )
                .context(format!("tracked_leaves[{index}].position")));
            }
            previous_position = Some(position);

            let decoder = tracked.decoder();
            let leaf = decode!(decoder, tracked.leaf)?;
            let path = tracked
                .path
                .into_iter()
                .enumerate()
                .map(|(path_index, node)| {
                    Word::try_from(node)
                        .context(format!("tracked_leaves[{index}].path[{path_index}]"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            mmr.track(position, leaf, &MerklePath::new(path))
                .map_err(ConversionError::new)
                .context(format!("tracked_leaves[{index}]"))?;
        }

        let mut previous_block_num = None;
        let block_headers = value
            .block_headers
            .into_iter()
            .enumerate()
            .map(|(index, header)| {
                let header =
                    BlockHeader::try_from(header).context(format!("block_headers[{index}]"))?;
                if previous_block_num.is_some_and(|previous| header.block_num() <= previous) {
                    return Err(ConversionError::message(
                        "block headers must be unique and ordered by ascending block number",
                    )
                    .context(format!("block_headers[{index}].block_num")));
                }
                previous_block_num = Some(header.block_num());
                Ok(header)
            })
            .collect::<Result<Vec<_>, ConversionError>>()?;

        Self::new(mmr, block_headers).map_err(ConversionError::new)
    }
}

// BLOCK HEADER
// ================================================================================================

impl From<&BlockHeader> for proto::blockchain::BlockHeader {
    fn from(header: &BlockHeader) -> Self {
        Self {
            version: header.version(),
            prev_block_commitment: Some(header.prev_block_commitment().into()),
            block_num: header.block_num().as_u32(),
            chain_commitment: Some(header.chain_commitment().into()),
            account_root: Some(header.account_root().into()),
            nullifier_root: Some(header.nullifier_root().into()),
            note_root: Some(header.note_root().into()),
            tx_commitment: Some(header.tx_commitment().into()),
            tx_kernel_commitment: Some(header.tx_kernel_commitment().into()),
            validator_keys: header.validator_keys().as_keys().iter().map(Into::into).collect(),
            timestamp: header.timestamp(),
            fee_parameters: Some(header.fee_parameters().into()),
        }
    }
}

impl From<BlockHeader> for proto::blockchain::BlockHeader {
    fn from(header: BlockHeader) -> Self {
        (&header).into()
    }
}

impl TryFrom<&proto::blockchain::BlockHeader> for BlockHeader {
    type Error = ConversionError;

    fn try_from(value: &proto::blockchain::BlockHeader) -> Result<Self, Self::Error> {
        value.try_into()
    }
}

impl TryFrom<proto::blockchain::BlockHeader> for BlockHeader {
    type Error = ConversionError;

    fn try_from(value: proto::blockchain::BlockHeader) -> Result<Self, Self::Error> {
        let decoder = value.decoder();
        let prev_block_commitment = decode!(decoder, value.prev_block_commitment)?;
        let chain_commitment = decode!(decoder, value.chain_commitment)?;
        let account_root = decode!(decoder, value.account_root)?;
        let nullifier_root = decode!(decoder, value.nullifier_root)?;
        let note_root = decode!(decoder, value.note_root)?;
        let tx_commitment = decode!(decoder, value.tx_commitment)?;
        let tx_kernel_commitment = decode!(decoder, value.tx_kernel_commitment)?;
        let validator_keys = value
            .validator_keys
            .into_iter()
            .map(PublicKey::try_from)
            .collect::<Result<Vec<_>, _>>()
            .context("validator_keys")?;
        let validator_keys = ValidatorKeys::new(validator_keys)
            .map_err(ConversionError::new)
            .context("validator_keys")?;
        let fee_parameters = decode!(decoder, value.fee_parameters)?;

        Ok(BlockHeader::new(
            value.version,
            prev_block_commitment,
            value.block_num.into(),
            chain_commitment,
            account_root,
            nullifier_root,
            note_root,
            tx_commitment,
            tx_kernel_commitment,
            validator_keys,
            fee_parameters,
            value.timestamp,
        ))
    }
}

// BLOCK BODY
// ================================================================================================

impl From<&BlockBody> for proto::blockchain::BlockBody {
    fn from(body: &BlockBody) -> Self {
        Self {
            contents: Some(proto::blockchain::BlockBodyContents {
                updated_accounts: body.updated_accounts().iter().map(Into::into).collect(),
                output_note_batches: body.output_note_batches().iter().map(Into::into).collect(),
                created_nullifiers: body
                    .created_nullifiers()
                    .iter()
                    .map(|nullifier| nullifier.as_word().into())
                    .collect(),
                transactions: body.transactions().as_slice().iter().map(Into::into).collect(),
            }),
        }
    }
}

impl From<BlockBody> for proto::blockchain::BlockBody {
    fn from(body: BlockBody) -> Self {
        (&body).into()
    }
}

impl TryFrom<&proto::blockchain::BlockBody> for BlockBody {
    type Error = ConversionError;

    fn try_from(value: &proto::blockchain::BlockBody) -> Result<Self, Self::Error> {
        value.try_into()
    }
}

impl TryFrom<proto::blockchain::BlockBody> for BlockBody {
    type Error = ConversionError;
    fn try_from(value: proto::blockchain::BlockBody) -> Result<Self, Self::Error> {
        let decoder = value.decoder();
        let contents: proto::blockchain::BlockBodyContents = decode!(decoder, value.contents)?;

        let updated_accounts = contents
            .updated_accounts
            .into_iter()
            .enumerate()
            .map(|(index, update)| {
                BlockAccountUpdate::try_from(update).context(format!("updated_accounts[{index}]"))
            })
            .collect::<Result<Vec<_>, _>>()?;

        if contents.output_note_batches.len() > MAX_BATCHES_PER_BLOCK {
            return Err(ConversionError::message(format!(
                "block has {} output note batches, maximum is {MAX_BATCHES_PER_BLOCK}",
                contents.output_note_batches.len()
            ))
            .context("output_note_batches"));
        }
        let output_note_batches = contents
            .output_note_batches
            .into_iter()
            .enumerate()
            .map(|(batch_index, batch)| {
                OutputNoteBatch::try_from(batch)
                    .and_then(|batch| {
                        for (note_index, _) in &batch {
                            if BlockNoteIndex::new(batch_index, *note_index).is_none() {
                                return Err(ConversionError::message(format!(
                                    "invalid block note index ({batch_index}, {note_index})"
                                )));
                            }
                        }
                        Ok(batch)
                    })
                    .context(format!("output_note_batches[{batch_index}]"))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let created_nullifiers = contents
            .created_nullifiers
            .into_iter()
            .enumerate()
            .map(|(index, nullifier)| {
                Word::try_from(nullifier)
                    .map(Nullifier::from_raw)
                    .context(format!("created_nullifiers[{index}]"))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let transactions = contents
            .transactions
            .into_iter()
            .enumerate()
            .map(|(index, transaction)| {
                TransactionHeader::try_from(transaction).context(format!("transactions[{index}]"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let transactions = OrderedTransactionHeaders::new_unchecked(transactions);

        Ok(BlockBody::new_unchecked(
            updated_accounts,
            output_note_batches,
            created_nullifiers,
            transactions,
        ))
    }
}

// BLOCK BODY COMPONENTS
// ================================================================================================

impl From<&BlockAccountUpdate> for proto::blockchain::BlockAccountUpdate {
    fn from(update: &BlockAccountUpdate) -> Self {
        Self {
            account_id: Some(update.account_id().into()),
            final_state_commitment: Some(update.final_state_commitment().into()),
            details: Some(update.details().into()),
        }
    }
}

impl TryFrom<proto::blockchain::BlockAccountUpdate> for BlockAccountUpdate {
    type Error = ConversionError;

    fn try_from(update: proto::blockchain::BlockAccountUpdate) -> Result<Self, Self::Error> {
        let decoder = update.decoder();
        let account_id: AccountId = decode!(decoder, update.account_id)?;
        let final_state_commitment = decode!(decoder, update.final_state_commitment)?;
        let details: AccountUpdateDetails = decode!(decoder, update.details)?;
        if let AccountUpdateDetails::Public(patch) = &details
            && patch.id() != account_id
        {
            return Err(ConversionError::message(format!(
                "public patch account ID {} does not match enclosing account ID {account_id}",
                patch.id()
            ))
            .context("details.public.account_id"));
        }

        Ok(BlockAccountUpdate::new(account_id, final_state_commitment, details))
    }
}

impl From<&(usize, OutputNote)> for proto::blockchain::IndexedOutputNote {
    fn from((index, note): &(usize, OutputNote)) -> Self {
        Self {
            note_index_in_batch: u32::try_from(*index)
                .expect("valid output note indices fit into u32"),
            note: Some(note.into()),
        }
    }
}

impl TryFrom<proto::blockchain::IndexedOutputNote> for (usize, OutputNote) {
    type Error = ConversionError;

    fn try_from(note: proto::blockchain::IndexedOutputNote) -> Result<Self, Self::Error> {
        let decoder = note.decoder();
        let index = usize::try_from(note.note_index_in_batch).context("note_index_in_batch")?;
        if index >= MAX_OUTPUT_NOTES_PER_BATCH {
            return Err(ConversionError::message(format!(
                "note index {index} exceeds maximum {}",
                MAX_OUTPUT_NOTES_PER_BATCH - 1
            ))
            .context("note_index_in_batch"));
        }
        let output_note = decode!(decoder, note.note)?;
        Ok((index, output_note))
    }
}

impl From<&OutputNoteBatch> for proto::blockchain::OutputNoteBatch {
    fn from(batch: &OutputNoteBatch) -> Self {
        Self {
            notes: batch.iter().map(Into::into).collect(),
        }
    }
}

impl TryFrom<proto::blockchain::OutputNoteBatch> for OutputNoteBatch {
    type Error = ConversionError;

    fn try_from(batch: proto::blockchain::OutputNoteBatch) -> Result<Self, Self::Error> {
        if batch.notes.len() > MAX_OUTPUT_NOTES_PER_BATCH {
            return Err(ConversionError::message(format!(
                "batch has {} notes, maximum is {MAX_OUTPUT_NOTES_PER_BATCH}",
                batch.notes.len()
            ))
            .context("notes"));
        }

        let mut indices = BTreeSet::new();
        batch
            .notes
            .into_iter()
            .enumerate()
            .map(|(position, note)| {
                let (index, note) =
                    <(usize, OutputNote)>::try_from(note).context(format!("notes[{position}]"))?;
                if !indices.insert(index) {
                    return Err(ConversionError::message(format!("duplicate note index {index}"))
                        .context(format!("notes[{position}].note_index_in_batch")));
                }
                Ok((index, note))
            })
            .collect()
    }
}

// BLOCK PROOF
// ================================================================================================

impl From<&BlockProof> for proto::blockchain::BlockProof {
    fn from(_proof: &BlockProof) -> Self {
        Self {}
    }
}

impl From<BlockProof> for proto::blockchain::BlockProof {
    fn from(proof: BlockProof) -> Self {
        Self::from(&proof)
    }
}

impl TryFrom<proto::blockchain::BlockProof> for BlockProof {
    type Error = ConversionError;

    fn try_from(_proof: proto::blockchain::BlockProof) -> Result<Self, Self::Error> {
        // BlockProof is currently an empty placeholder without a public production constructor.
        // Replace this isolated workaround with field-based construction when it gains fields.
        BlockProof::read_from_bytes(&[])
            .map_err(|source| ConversionError::deserialization("BlockProof", source))
    }
}

// SIGNED BLOCK
// ================================================================================================

impl From<&SignedBlock> for proto::blockchain::SignedBlock {
    fn from(block: &SignedBlock) -> Self {
        Self {
            header: Some(block.header().into()),
            body: Some(block.body().into()),
            signatures: block.signatures().as_signatures().iter().map(Into::into).collect(),
        }
    }
}

impl From<SignedBlock> for proto::blockchain::SignedBlock {
    fn from(block: SignedBlock) -> Self {
        (&block).into()
    }
}

impl TryFrom<&proto::blockchain::SignedBlock> for SignedBlock {
    type Error = ConversionError;

    fn try_from(value: &proto::blockchain::SignedBlock) -> Result<Self, Self::Error> {
        value.try_into()
    }
}

impl TryFrom<proto::blockchain::SignedBlock> for SignedBlock {
    type Error = ConversionError;
    fn try_from(value: proto::blockchain::SignedBlock) -> Result<Self, Self::Error> {
        let decoder = value.decoder();
        let header: BlockHeader = decode!(decoder, value.header)?;
        let body: BlockBody = decode!(decoder, value.body)?;
        let signatures = value
            .signatures
            .into_iter()
            .map(Signature::try_from)
            .collect::<Result<Vec<_>, _>>()
            .context("signatures")?;
        let signatures = BlockSignatures::new(signatures)
            .map_err(ConversionError::new)
            .context("signatures")?;

        if header.tx_commitment() != body.transaction_commitment() {
            return Err(ConversionError::message(format!(
                "header transaction commitment {} does not match body transaction commitment {}",
                header.tx_commitment(),
                body.transaction_commitment(),
            ))
            .context("tx_commitment")
            .context("header"));
        }

        let body_note_root = body.compute_block_note_tree().root();
        if header.note_root() != body_note_root {
            return Err(ConversionError::message(format!(
                "header note root {} does not match body note root {body_note_root}",
                header.note_root(),
            ))
            .context("note_root")
            .context("header"));
        }

        // This establishes header/body self-consistency. Parent/signature trust is validated by the
        // state application path against an already trusted parent header.
        SignedBlock::new(header, body, signatures)
            .map_err(ConversionError::new)
            .context("body")
    }
}

// PUBLIC KEY
// ================================================================================================

impl TryFrom<proto::blockchain::ValidatorPublicKey> for PublicKey {
    type Error = ConversionError;
    fn try_from(public_key: proto::blockchain::ValidatorPublicKey) -> Result<Self, Self::Error> {
        PublicKey::decode_bytes(&public_key.validator_key, "PublicKey")
    }
}

impl From<PublicKey> for proto::blockchain::ValidatorPublicKey {
    fn from(value: PublicKey) -> Self {
        Self::from(&value)
    }
}

impl From<&PublicKey> for proto::blockchain::ValidatorPublicKey {
    fn from(value: &PublicKey) -> Self {
        Self { validator_key: value.to_bytes() }
    }
}

// SIGNATURE
// ================================================================================================

impl TryFrom<proto::blockchain::BlockSignature> for Signature {
    type Error = ConversionError;
    fn try_from(signature: proto::blockchain::BlockSignature) -> Result<Self, Self::Error> {
        Signature::decode_bytes(&signature.signature, "Signature")
    }
}

impl From<Signature> for proto::blockchain::BlockSignature {
    fn from(value: Signature) -> Self {
        Self::from(&value)
    }
}

impl From<&Signature> for proto::blockchain::BlockSignature {
    fn from(value: &Signature) -> Self {
        Self { signature: value.to_bytes() }
    }
}

// FEE PARAMETERS
// ================================================================================================

impl TryFrom<proto::blockchain::FeeParameters> for FeeParameters {
    type Error = ConversionError;
    fn try_from(fee_params: proto::blockchain::FeeParameters) -> Result<Self, Self::Error> {
        let decoder = fee_params.decoder();
        let native_asset_id = decode!(decoder, fee_params.native_asset_id)?;
        Ok(FeeParameters::new(native_asset_id, fee_params.verification_base_fee))
    }
}

impl From<FeeParameters> for proto::blockchain::FeeParameters {
    fn from(value: FeeParameters) -> Self {
        Self::from(&value)
    }
}

impl From<&FeeParameters> for proto::blockchain::FeeParameters {
    fn from(value: &FeeParameters) -> Self {
        Self {
            native_asset_id: Some(value.fee_faucet_id().into()),
            verification_base_fee: value.verification_base_fee(),
        }
    }
}

// BLOCK RANGE
// ================================================================================================

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum InvalidBlockRange {
    #[error("start ({start}) greater than end ({end})")]
    StartGreaterThanEnd { start: BlockNumber, end: BlockNumber },
    #[error("empty range: start ({start})..end ({end})")]
    EmptyRange { start: BlockNumber, end: BlockNumber },
}

impl proto::rpc::BlockRange {
    /// Converts the block range into an inclusive range.
    pub fn into_inclusive_range<T: From<InvalidBlockRange>>(
        self,
    ) -> Result<RangeInclusive<BlockNumber>, T> {
        let block_range = RangeInclusive::new(self.block_from.into(), self.block_to.into());

        if block_range.start() > block_range.end() {
            return Err(InvalidBlockRange::StartGreaterThanEnd {
                start: *block_range.start(),
                end: *block_range.end(),
            }
            .into());
        }

        if block_range.is_empty() {
            return Err(InvalidBlockRange::EmptyRange {
                start: *block_range.start(),
                end: *block_range.end(),
            }
            .into());
        }

        Ok(block_range)
    }
}

impl From<RangeInclusive<BlockNumber>> for proto::rpc::BlockRange {
    fn from(range: RangeInclusive<BlockNumber>) -> Self {
        Self {
            block_from: range.start().as_u32(),
            block_to: range.end().as_u32(),
        }
    }
}

#[cfg(test)]
mod tests {
    use miden_protocol::account::{
        AccountId,
        AccountIdVersion,
        AccountType,
        AccountUpdateDetails,
        AssetCallbackFlag,
    };
    use miden_protocol::block::{
        BlockAccountUpdate,
        BlockBody,
        BlockHeader,
        BlockProof,
        BlockSignatures,
        SignedBlock,
    };
    use miden_protocol::crypto::merkle::mmr::{Mmr, PartialMmr};
    use miden_protocol::note::{Note, NoteAttachments, Nullifier};
    use miden_protocol::transaction::{
        OrderedTransactionHeaders,
        OutputNote,
        PartialBlockchain,
        PrivateOutputNote,
    };
    use miden_protocol::utils::serde::Serializable;
    use miden_protocol::{MAX_BATCHES_PER_BLOCK, MAX_OUTPUT_NOTES_PER_BATCH, Word};
    use prost::Message;

    use crate::generated as proto;

    fn empty_body() -> BlockBody {
        BlockBody::new_unchecked(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            OrderedTransactionHeaders::new_unchecked(Vec::new()),
        )
    }

    fn header_for(body: &BlockBody) -> BlockHeader {
        let template = BlockHeader::mock(
            1,
            None,
            Some(body.compute_block_note_tree().root()),
            &[],
            Word::default(),
        );
        BlockHeader::new(
            template.version(),
            template.prev_block_commitment(),
            template.block_num(),
            template.chain_commitment(),
            template.account_root(),
            template.nullifier_root(),
            body.compute_block_note_tree().root(),
            body.transaction_commitment(),
            template.tx_kernel_commitment(),
            template.validator_keys().clone(),
            template.fee_parameters().clone(),
            template.timestamp(),
        )
    }

    fn private_output_note(serial_num: Word) -> OutputNote {
        let note = Note::mock_noop(serial_num);
        OutputNote::Private(
            PrivateOutputNote::new(*note.header(), NoteAttachments::default())
                .expect("mock note is private"),
        )
    }

    #[test]
    fn empty_block_body_roundtrips_structurally() {
        let body = empty_body();
        let encoded = proto::blockchain::BlockBody::from(&body);

        assert!(encoded.contents.is_some());
        assert_eq!(BlockBody::try_from(encoded).unwrap(), body);
    }

    #[test]
    fn partial_blockchain_roundtrip_preserves_tracked_leaves_without_headers() {
        let leaves = [
            Word::from([1_u32, 2, 3, 4]),
            Word::from([5_u32, 6, 7, 8]),
            Word::from([9_u32, 10, 11, 12]),
        ];
        let mmr = Mmr::try_from_iter(leaves).unwrap();
        let mut partial = PartialMmr::from_peaks(mmr.peaks());
        let proof = mmr.open(0).unwrap();
        partial.track(0, proof.leaf(), proof.merkle_path()).unwrap();
        let chain = PartialBlockchain::new(partial, Vec::new()).unwrap();

        let encoded = proto::blockchain::PartialBlockchain::from(&chain);
        assert_eq!(encoded.tracked_leaves.len(), 1);
        assert!(encoded.block_headers.is_empty());
        assert_eq!(PartialBlockchain::try_from(encoded).unwrap(), chain);
    }

    #[test]
    fn representative_block_body_preserves_order_and_sparse_note_indices() {
        let account_id = AccountId::dummy(
            [7; 15],
            AccountIdVersion::Version1,
            AccountType::Private,
            AssetCallbackFlag::Disabled,
        );
        let account_update = BlockAccountUpdate::new(
            account_id,
            Word::from([1_u32, 2, 3, 4]),
            AccountUpdateDetails::Private,
        );
        let body = BlockBody::new_unchecked(
            vec![account_update],
            vec![vec![(3, private_output_note(Word::from([5_u32, 6, 7, 8])))], vec![]],
            vec![
                Nullifier::from_raw(Word::from([9_u32, 10, 11, 12])),
                Nullifier::from_raw(Word::from([13_u32, 14, 15, 16])),
            ],
            OrderedTransactionHeaders::new_unchecked(Vec::new()),
        );
        let expected_indices = body.output_notes().map(|(index, _)| index).collect::<Vec<_>>();
        let expected_tx_commitment = body.transaction_commitment();
        let expected_note_root = body.compute_block_note_tree().root();

        let proto_body = proto::blockchain::BlockBody::from(&body);
        let decoded = BlockBody::try_from(proto_body.clone()).unwrap();

        assert_eq!(decoded, body);
        assert_eq!(decoded.transaction_commitment(), expected_tx_commitment);
        assert_eq!(decoded.compute_block_note_tree().root(), expected_note_root);
        assert_eq!(
            decoded.output_notes().map(|(index, _)| index).collect::<Vec<_>>(),
            expected_indices
        );
        assert!(proto_body.encoded_len() > 0, "structured body should have a wire payload");
    }

    #[test]
    fn block_body_requires_contents() {
        let error = BlockBody::try_from(proto::blockchain::BlockBody { contents: None })
            .unwrap_err()
            .to_string();
        assert!(error.contains("contents"));
    }

    #[test]
    fn output_note_batches_reject_duplicate_and_out_of_range_indices() {
        let note = proto::transaction::OutputNote::from(&private_output_note(Word::default()));
        let duplicate = proto::blockchain::OutputNoteBatch {
            notes: vec![
                proto::blockchain::IndexedOutputNote {
                    note_index_in_batch: 2,
                    note: Some(note.clone()),
                },
                proto::blockchain::IndexedOutputNote {
                    note_index_in_batch: 2,
                    note: Some(note.clone()),
                },
            ],
        };
        let error = miden_protocol::block::OutputNoteBatch::try_from(duplicate)
            .unwrap_err()
            .to_string();
        assert!(error.contains("notes[1].note_index_in_batch"));

        let out_of_range = proto::blockchain::OutputNoteBatch {
            notes: vec![proto::blockchain::IndexedOutputNote {
                note_index_in_batch: u32::try_from(MAX_OUTPUT_NOTES_PER_BATCH).unwrap(),
                note: Some(note),
            }],
        };
        let error = miden_protocol::block::OutputNoteBatch::try_from(out_of_range)
            .unwrap_err()
            .to_string();
        assert!(error.contains("note_index_in_batch"));
    }

    #[test]
    fn block_body_rejects_too_many_batches() {
        let contents = proto::blockchain::BlockBodyContents {
            output_note_batches: vec![
                proto::blockchain::OutputNoteBatch::default();
                MAX_BATCHES_PER_BLOCK + 1
            ],
            ..Default::default()
        };
        let error = BlockBody::try_from(proto::blockchain::BlockBody { contents: Some(contents) })
            .unwrap_err()
            .to_string();
        assert!(error.contains("output_note_batches"));
    }

    #[test]
    fn signed_block_roundtrips_and_rejects_commitment_mismatches() {
        let body = empty_body();
        let header = header_for(&body);
        let block =
            SignedBlock::new(header, body, BlockSignatures::new(Vec::new()).unwrap()).unwrap();
        let encoded = proto::blockchain::SignedBlock::from(&block);

        assert_eq!(SignedBlock::try_from(encoded.clone()).unwrap(), block);

        let mut bad_tx_commitment = encoded.clone();
        bad_tx_commitment.header.as_mut().unwrap().tx_commitment =
            Some(Word::from([1_u32, 0, 0, 0]).into());
        let error = SignedBlock::try_from(bad_tx_commitment).unwrap_err().to_string();
        assert!(error.contains("header.tx_commitment"));

        let mut bad_note_root = encoded;
        bad_note_root.header.as_mut().unwrap().note_root =
            Some(Word::from([1_u32, 0, 0, 0]).into());
        let error = SignedBlock::try_from(bad_note_root).unwrap_err().to_string();
        assert!(error.contains("header.note_root"));
    }

    #[test]
    fn block_proof_roundtrips_and_keeps_presence_distinct() {
        let proof = BlockProof::new_dummy();
        assert!(
            proof.to_bytes().is_empty(),
            "update the protobuf proof envelope when BlockProof changes"
        );

        let encoded = proto::blockchain::BlockProof::from(&proof);
        assert_eq!(BlockProof::try_from(encoded).unwrap(), proof);

        let absent = proto::blockchain::MaybeBlock::default();
        let present = proto::blockchain::MaybeBlock {
            block_proof: Some(encoded),
            ..Default::default()
        };
        assert!(absent.block_proof.is_none());
        assert!(present.block_proof.is_some());
    }
}
