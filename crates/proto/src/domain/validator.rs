use std::collections::BTreeMap;

use miden_protocol::account::AccountId;
use miden_protocol::block::account_tree::AccountWitness;
use miden_protocol::block::nullifier_tree::NullifierWitness;
use miden_protocol::block::{BlockInputs, ProposedBlock, ValidatorKeys};
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::PublicKey;
use miden_protocol::crypto::merkle::smt::SmtProof;
use miden_protocol::note::{NoteId, NoteInclusionProof, Nullifier};
use miden_protocol::{MAX_BATCHES_PER_BLOCK, Word};

use crate::decode::{ConversionResultExt, GrpcDecodeExt};
use crate::domain::account::AccountWitnessRecord;
use crate::domain::batch::decode_standalone_proven_batch;
use crate::errors::ConversionError;
use crate::{decode, generated as proto};

impl From<(&ProposedBlock, &BlockInputs)> for proto::validator::ProposedBlock {
    fn from((block, inputs): (&ProposedBlock, &BlockInputs)) -> Self {
        Self {
            block_inputs: Some(inputs.into()),
            batches: block.batches().as_slice().iter().map(Into::into).collect(),
            timestamp: block.timestamp(),
            next_validator_keys: block
                .next_validator_keys()
                .as_keys()
                .iter()
                .map(Into::into)
                .collect(),
        }
    }
}

impl From<&BlockInputs> for proto::validator::BlockInputs {
    fn from(value: &BlockInputs) -> Self {
        Self {
            prev_block_header: Some(value.prev_block_header().into()),
            partial_blockchain: Some(value.partial_blockchain().into()),
            account_witnesses: value
                .account_witnesses()
                .iter()
                .map(|(account_id, witness)| {
                    AccountWitnessRecord {
                        account_id: *account_id,
                        witness: witness.clone(),
                    }
                    .into()
                })
                .collect(),
            nullifier_witnesses: value
                .nullifier_witnesses()
                .iter()
                .map(|(nullifier, witness)| proto::validator::NullifierWitness {
                    nullifier: Some(nullifier.as_word().into()),
                    proof: Some(witness.proof().clone().into()),
                })
                .collect(),
            unauthenticated_note_proofs: value
                .unauthenticated_note_proofs()
                .iter()
                .map(Into::into)
                .collect(),
        }
    }
}

impl TryFrom<proto::validator::BlockInputs> for BlockInputs {
    type Error = ConversionError;

    fn try_from(value: proto::validator::BlockInputs) -> Result<Self, Self::Error> {
        let decoder = value.decoder();
        let prev_block_header = decode!(decoder, value.prev_block_header)?;
        let partial_blockchain = decode!(decoder, value.partial_blockchain)?;

        let mut account_witnesses = BTreeMap::<AccountId, AccountWitness>::new();
        let mut previous_account_id = None;
        for (index, witness) in value.account_witnesses.into_iter().enumerate() {
            let record = AccountWitnessRecord::try_from(witness)
                .context(format!("account_witnesses[{index}]"))?;
            if previous_account_id.is_some_and(|previous| record.account_id <= previous) {
                return Err(ConversionError::message(
                    "account witnesses must have unique, ascending requested account IDs",
                )
                .context(format!("account_witnesses[{index}].account_id")));
            }
            previous_account_id = Some(record.account_id);
            account_witnesses.insert(record.account_id, record.witness);
        }

        let mut nullifier_witnesses = BTreeMap::new();
        let mut previous_nullifier = None;
        for (index, witness) in value.nullifier_witnesses.into_iter().enumerate() {
            let decoder = witness.decoder();
            let word: Word = decode!(decoder, witness.nullifier)?;
            let nullifier = Nullifier::from_raw(word);
            if previous_nullifier.is_some_and(|previous| nullifier <= previous) {
                return Err(ConversionError::message(
                    "nullifier witnesses must have unique, ascending nullifiers",
                )
                .context(format!("nullifier_witnesses[{index}].nullifier")));
            }
            previous_nullifier = Some(nullifier);
            let proof: SmtProof = decode!(decoder, witness.proof)?;
            if proof.get(&nullifier.as_word()).is_none() {
                return Err(ConversionError::message("SMT opening does not contain the nullifier")
                    .context(format!("nullifier_witnesses[{index}].proof")));
            }
            nullifier_witnesses.insert(nullifier, NullifierWitness::new(proof));
        }

        let mut unauthenticated_note_proofs = BTreeMap::<NoteId, NoteInclusionProof>::new();
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

        Ok(BlockInputs::new(
            prev_block_header,
            partial_blockchain,
            account_witnesses,
            nullifier_witnesses,
            unauthenticated_note_proofs,
        ))
    }
}

impl TryFrom<proto::validator::ProposedBlock> for ProposedBlock {
    type Error = ConversionError;

    fn try_from(value: proto::validator::ProposedBlock) -> Result<Self, Self::Error> {
        if value.batches.len() > MAX_BATCHES_PER_BLOCK {
            return Err(ConversionError::message("too many batches").context("batches"));
        }
        let decoder = value.decoder();
        let block_inputs = decode!(decoder, value.block_inputs)?;
        let batches = value
            .batches
            .into_iter()
            .enumerate()
            .map(|(index, batch)| {
                decode_standalone_proven_batch(batch).context(format!("batches[{index}]"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let next_validator_keys = value
            .next_validator_keys
            .into_iter()
            .enumerate()
            .map(|(index, key)| {
                PublicKey::try_from(key).context(format!("next_validator_keys[{index}]"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let next_validator_keys = ValidatorKeys::new(next_validator_keys)
            .map_err(ConversionError::new)
            .context("next_validator_keys")?;

        ProposedBlock::new_at(block_inputs, batches, value.timestamp)
            .map(|block| block.with_next_validator_keys(next_validator_keys))
            .map_err(ConversionError::new)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use miden_protocol::Word;
    use miden_protocol::block::{BlockHeader, BlockInputs, ProposedBlock, ValidatorKeys};
    use miden_protocol::testing::random_secret_key::random_secret_key;
    use miden_protocol::transaction::PartialBlockchain;

    use crate::generated as proto;

    #[test]
    fn proposed_block_roundtrips_with_explicit_timestamp_and_validator_rotation() {
        let partial_blockchain = PartialBlockchain::default();
        let parent = BlockHeader::mock(
            0,
            Some(partial_blockchain.peaks().hash_peaks()),
            None,
            &[],
            Word::empty(),
        );
        let inputs = BlockInputs::new(
            parent.clone(),
            partial_blockchain,
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        );
        let timestamp = parent.timestamp().saturating_add(1);
        let next_validator_keys =
            ValidatorKeys::new(vec![random_secret_key().public_key()]).unwrap();
        let block = ProposedBlock::new_at(inputs.clone(), vec![], timestamp)
            .unwrap()
            .with_next_validator_keys(next_validator_keys.clone());

        let encoded = proto::validator::ProposedBlock::from((&block, &inputs));
        let decoded = ProposedBlock::try_from(encoded).unwrap();

        assert_eq!(decoded.timestamp(), timestamp);
        assert_eq!(decoded.next_validator_keys(), &next_validator_keys);
        let expected = block.into_header_and_body().unwrap();
        let actual = decoded.into_header_and_body().unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn proposed_block_rejects_missing_inputs_and_duplicate_validator_keys() {
        let missing_inputs = proto::validator::ProposedBlock::default();
        assert!(
            ProposedBlock::try_from(missing_inputs)
                .unwrap_err()
                .to_string()
                .contains("block_inputs")
        );

        let partial_blockchain = PartialBlockchain::default();
        let parent = BlockHeader::mock(
            0,
            Some(partial_blockchain.peaks().hash_peaks()),
            None,
            &[],
            Word::empty(),
        );
        let inputs = BlockInputs::new(
            parent.clone(),
            partial_blockchain,
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        );
        let block =
            ProposedBlock::new_at(inputs.clone(), vec![], parent.timestamp().saturating_add(1))
                .unwrap();
        let mut encoded = proto::validator::ProposedBlock::from((&block, &inputs));
        encoded.next_validator_keys.push(encoded.next_validator_keys[0].clone());

        assert!(
            ProposedBlock::try_from(encoded)
                .unwrap_err()
                .to_string()
                .contains("next_validator_keys")
        );
    }
}
