use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use miden_node_proto::clients::{Builder, ValidatorClient};
use miden_node_proto::generated::validator::{BlockSubscriptionRequest, BlockSubscriptionResponse};
use miden_node_store::State;
use miden_node_store::state::Finality;
use miden_protocol::Word;
use miden_protocol::block::{
    BlockBody,
    BlockHeader,
    BlockNumber,
    BlockSignatures,
    SignedBlock,
    ValidatorKeys,
};
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::Signature;
use miden_protocol::utils::serde::Deserializable;
use tokio_stream::StreamExt;
use tonic::codec::Streaming;
use tracing::info;
use url::Url;

use super::ENV_DATA_DIRECTORY;
use super::store::StoreOptions;
use crate::LOG_TARGET;

// RECOVER
// ================================================================================================

#[derive(clap::Args, Clone, Debug)]
pub struct RecoverCommand {
    /// Directory containing the node's local data storage.
    #[arg(long, env = ENV_DATA_DIRECTORY, value_name = "DIR")]
    data_directory: PathBuf,

    /// The validator service gRPC URLs to recover blocks from.
    ///
    /// Repeat the flag once per validator. The URLs must cover the full validator set, since each
    /// validator's block backup carries only that validator's own signature and a block can only
    /// be reconstructed once the signatures of the whole set have been collected. The environment
    /// variable accepts a comma-separated list.
    #[arg(
        long = "validator.url",
        env = "MIDEN_NODE_VALIDATOR_URL",
        value_name = "URL",
        value_delimiter = ',',
        required = true
    )]
    validator_urls: Vec<Url>,

    #[command(flatten)]
    store: StoreOptions,
}

impl RecoverCommand {
    pub async fn handle(self) -> anyhow::Result<()> {
        let state = self.load_state().await?;
        let validators = self
            .validator_urls
            .iter()
            .map(|url| Ok((url.clone(), Self::validator_client(url)?)))
            .collect::<anyhow::Result<Vec<_>>>()?;
        recover_from_validators(&state, validators).await
    }

    async fn load_state(&self) -> anyhow::Result<Arc<State>> {
        let state = State::load_with_database_options(
            &self.data_directory,
            self.store.storage.clone().into(),
            self.store.sqlite.database_options(),
        )
        .await
        .context("failed to load state")?;

        Ok(Arc::new(state))
    }

    fn validator_client(url: &Url) -> anyhow::Result<ValidatorClient> {
        Ok(Builder::new(url.clone())
            .with_tls()?
            .with_timeout(Duration::from_secs(5))
            .without_metadata_version()
            .without_metadata_genesis()
            .with_otel_context_injection()
            .connect_lazy::<ValidatorClient>())
    }
}

/// Streams blocks from all validators into the local store until the recovery target is reached.
///
/// Each validator's block backup carries only that validator's own signature, so blocks are pulled
/// from every validator in lock-step and the individual signatures are coalesced into the full,
/// positionally ordered signature set before each block is applied.
async fn recover_from_validators(
    state: &Arc<State>,
    mut validators: Vec<(Url, ValidatorClient)>,
) -> anyhow::Result<()> {
    // Capture the smallest of the validators' chain tips as the recovery target: it is the highest
    // block for which every validator can supply its signature. The validators' block streams
    // follow live blocks indefinitely, so without a fixed target we could never tell when to stop.
    // The sequencer must be shut down during recovery, so these tips do not advance.
    let mut recovery_tip: Option<BlockNumber> = None;
    for (url, validator) in &mut validators {
        let tip = BlockNumber::from(
            validator
                .status(())
                .await
                .with_context(|| format!("failed to query status of validator {url}"))?
                .into_inner()
                .chain_tip,
        );
        recovery_tip = Some(recovery_tip.map_or(tip, |current| current.min(tip)));
    }
    let recovery_tip = recovery_tip.context("at least one validator URL is required")?;

    let local_tip = state.chain_tip(Finality::Committed).await;
    if local_tip >= recovery_tip {
        info!(
            target: LOG_TARGET,
            local_tip = local_tip.as_u32(),
            recovery_tip = recovery_tip.as_u32(),
            "Local chain is already at the validators' chain tip; nothing to recover",
        );
        return Ok(());
    }

    // The parent of the first recovered block is the local chain tip; each parent's header commits
    // to the validator keys whose signatures the child block must carry.
    let (parent, _) = state
        .get_block_header(Some(local_tip), false)
        .await
        .context("failed to load the local chain tip header")?;
    let mut parent = parent.context("local chain tip header not found")?;

    let block_from = local_tip.child().as_u32();
    info!(
        target: LOG_TARGET,
        block_from,
        recovery_tip = recovery_tip.as_u32(),
        validators = validators.len(),
        "Recovering blocks from validators",
    );

    let mut streams = Vec::with_capacity(validators.len());
    for (url, validator) in &mut validators {
        let stream = validator
            .block_subscription(BlockSubscriptionRequest { block_from })
            .await
            .with_context(|| format!("failed to open block subscription on validator {url}"))?
            .into_inner();
        streams.push((url.clone(), stream));
    }

    for block_num in block_from..=recovery_tip.as_u32() {
        let block = next_coalesced_block(&mut streams, &parent)
            .await
            .with_context(|| format!("failed to reconstruct block {block_num}"))?;
        parent = block.header().clone();
        state.apply_block(block).await.context("failed to apply recovered block")?;
        info!(target: LOG_TARGET, block_number = block_num, "Applied recovered block");
    }

    info!(target: LOG_TARGET, chain_tip = recovery_tip.as_u32(), "Block recovery complete");
    Ok(())
}

/// Pulls the next block from every validator stream and coalesces the validators' individual
/// signatures into a fully signed block, validated against the trusted `parent` header.
///
/// Every validator serves the same header and body for a committed block, but each backup carries
/// exactly one signature — the validator's own — and does not record which position in the
/// validator set that signature occupies. The signatures are therefore matched to their positions
/// by verifying them against the validator keys committed to by the parent header.
async fn next_coalesced_block(
    streams: &mut [(Url, Streaming<BlockSubscriptionResponse>)],
    parent: &BlockHeader,
) -> anyhow::Result<SignedBlock> {
    let expected_block_num = parent.block_num().child();

    let mut signatures = Vec::with_capacity(streams.len());
    let mut first_parts: Option<(BlockHeader, BlockBody)> = None;
    for (url, stream) in streams.iter_mut() {
        let event = stream
            .next()
            .await
            .with_context(|| {
                format!("block stream of validator {url} ended before the recovery target")
            })?
            .with_context(|| format!("block stream of validator {url} returned an error"))?;
        let block = SignedBlock::read_from_bytes(&event.block)
            .with_context(|| format!("failed to deserialize block from validator {url}"))?;

        anyhow::ensure!(
            block.header().block_num() == expected_block_num,
            "validator {url} served block {} but block {expected_block_num} was expected",
            block.header().block_num(),
        );

        let (header, body, block_signatures) = block.into_parts();
        let signature = match block_signatures.as_signatures() {
            [signature] => signature.clone(),
            other => anyhow::bail!(
                "backup block of validator {url} carries {} signatures but exactly one (its own) was expected",
                other.len(),
            ),
        };

        match &first_parts {
            None => first_parts = Some((header, body)),
            Some((first_header, _)) => anyhow::ensure!(
                header.commitment() == first_header.commitment(),
                "validator {url} served a block with commitment {} which diverges from commitment {} served by the first validator",
                header.commitment(),
                first_header.commitment(),
            ),
        }
        signatures.push(signature);
    }
    let (header, body) = first_parts.context("at least one validator stream is required")?;

    let signatures = coalesce_signatures(signatures, header.commitment(), parent.validator_keys())?;
    let block = SignedBlock::new_unchecked(header, body, signatures);

    // Fully validate the reconstructed block before it is persisted, including verifying the
    // signature set against the parent's validator keys; `State::apply_block` does not verify
    // signatures.
    block.validate(Some(parent)).context("coalesced block failed validation")?;

    Ok(block)
}

/// Orders the collected per-validator signatures positionally against the validator set: the
/// signature in slot `i` must be produced by the validator key at index `i`.
///
/// A backup does not record which position its signature occupies, so each signature is matched to
/// its position by verifying it against the validator keys over the block commitment.
fn coalesce_signatures(
    mut signatures: Vec<Signature>,
    block_commitment: Word,
    validator_keys: &ValidatorKeys,
) -> anyhow::Result<BlockSignatures> {
    anyhow::ensure!(
        signatures.len() == validator_keys.len(),
        "collected {} signatures but the validator set has {} keys; the provided validator URLs \
         must cover the full validator set",
        signatures.len(),
        validator_keys.len(),
    );

    let mut ordered = Vec::with_capacity(validator_keys.len());
    for (position, key) in validator_keys.as_keys().iter().enumerate() {
        let matched = signatures
            .iter()
            .position(|signature| signature.verify(block_commitment, key))
            .with_context(|| {
                format!(
                    "no unmatched signature verifies against the validator key at position \
                     {position}; the provided validator URLs must cover the full validator set"
                )
            })?;
        ordered.push(signatures.swap_remove(matched));
    }

    BlockSignatures::new(ordered).context("failed to construct the coalesced signature set")
}

#[cfg(test)]
mod tests {
    use miden_protocol::Word;
    use miden_protocol::block::ValidatorKeys;
    use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey;

    use super::coalesce_signatures;

    fn signers(count: usize) -> Vec<SigningKey> {
        (0..count).map(|_| SigningKey::new()).collect()
    }

    fn validator_keys(signers: &[SigningKey]) -> ValidatorKeys {
        ValidatorKeys::new(signers.iter().map(SigningKey::public_key).collect()).unwrap()
    }

    #[test]
    fn signatures_are_ordered_against_the_validator_set() {
        let commitment = Word::from([1u32, 2, 3, 4]);
        let signers = signers(3);
        let keys = validator_keys(&signers);

        // Collect the signatures in reverse order to prove they get reordered.
        let shuffled = signers.iter().rev().map(|signer| signer.sign(commitment)).collect();

        let coalesced = coalesce_signatures(shuffled, commitment, &keys).unwrap();
        coalesced.verify_against(commitment, &keys).unwrap();
    }

    #[test]
    fn signature_count_mismatch_is_rejected() {
        let commitment = Word::from([1u32, 2, 3, 4]);
        let signers = signers(3);
        let keys = validator_keys(&signers);

        let missing_one = signers.iter().take(2).map(|signer| signer.sign(commitment)).collect();

        let err = coalesce_signatures(missing_one, commitment, &keys).unwrap_err();
        assert!(err.to_string().contains("collected 2 signatures"), "{err}");
    }

    #[test]
    fn duplicate_validator_signature_is_rejected() {
        let commitment = Word::from([1u32, 2, 3, 4]);
        let signers = signers(3);
        let keys = validator_keys(&signers);

        // Two streams served by the same validator: its signature appears twice and the third
        // validator's signature is missing.
        let duplicated = vec![
            signers[0].sign(commitment),
            signers[0].sign(commitment),
            signers[1].sign(commitment),
        ];

        let err = coalesce_signatures(duplicated, commitment, &keys).unwrap_err();
        assert!(err.to_string().contains("no unmatched signature verifies"), "{err}");
    }

    #[test]
    fn foreign_signature_is_rejected() {
        let commitment = Word::from([1u32, 2, 3, 4]);
        let signers = signers(3);
        let keys = validator_keys(&signers);

        // One signature comes from a key outside the validator set.
        let with_foreign = vec![
            signers[0].sign(commitment),
            signers[1].sign(commitment),
            SigningKey::new().sign(commitment),
        ];

        let err = coalesce_signatures(with_foreign, commitment, &keys).unwrap_err();
        assert!(err.to_string().contains("no unmatched signature verifies"), "{err}");
    }
}
