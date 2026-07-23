use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use miden_node_db::DatabaseError;
use miden_node_db::sqlite::Database;
use miden_node_store::BlockStore;
use miden_node_utils::shutdown::CancellationToken;
use miden_node_utils::tracing::{miden_instrument, miden_span_record};
use miden_protocol::Word;
use miden_protocol::block::{
    BlockHeader,
    BlockNumber,
    BlockSignatures,
    ProposedBlock,
    SignedBlock,
};
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::{PublicKey, Signature};
use miden_protocol::crypto::utils::Serializable;
use miden_protocol::errors::ProposedBlockError;
use miden_protocol::transaction::{TransactionHeader, TransactionId};
use tokio::sync::{Semaphore, watch};

use crate::db::{
    ArchivedEncryptionKey,
    find_unvalidated_transactions,
    insert_encryption_key,
    load_block_header,
    load_chain_tip,
    max_archived_encryption_key_epoch,
};
use crate::signers::EncryptionKeySet;
use crate::{COMPONENT, LOG_TARGET, TransactionInputDecrypter, ValidatorSigner};

#[cfg(test)]
mod tests;

mod block_subscription;
mod get_transaction_encryption_key;
mod sign_block;
mod status;
mod submit_proven_transaction;

// VALIDATOR ERROR
// ================================================================================================

#[derive(thiserror::Error, Debug)]
pub enum ValidatorError {
    #[error("block contains unvalidated transactions {0:?}")]
    UnvalidatedTransactions(Vec<TransactionId>),
    #[error("failed to build block")]
    BlockBuildingFailed(#[source] ProposedBlockError),
    #[error("failed to sign block: {0}")]
    BlockSigningFailed(String),
    #[error("failed to select transactions")]
    DatabaseError(#[source] DatabaseError),
    #[error("block number mismatch: expected {expected}, got {actual}")]
    BlockNumberMismatch {
        expected: BlockNumber,
        actual: BlockNumber,
    },
    #[error("previous block commitment does not match chain tip")]
    PrevBlockCommitmentMismatch,
    #[error("no previous block header available for chain tip overwrite")]
    NoPrevBlockHeader,
    #[error(
        "validator signing key {actual:?} does not match the block's validator key {expected:?}"
    )]
    ValidatorKeyMismatch { expected: PublicKey, actual: PublicKey },
    #[error("no chain tip exists")]
    NoChainTip,
    #[error("failed to backup block")]
    BlockBackupFailed(#[source] std::io::Error),
    #[error("expected a single-key validator set, got {actual} keys")]
    UnexpectedValidatorSetSize { actual: usize },
    #[error("no genesis block header exists")]
    NoGenesisHeader,
    #[error("failed to attest the transaction encryption key: {0}")]
    EncryptionKeyAttestationFailed(String),
    #[error("failed to archive the transaction encryption key: {0}")]
    EncryptionKeyArchivalFailed(String),
}

// ATTESTED ENCRYPTION KEYS
// ================================================================================

/// The encryption keys of one epoch together with this validator's attestations over them.
pub(crate) struct AttestedEncryptionKeys {
    /// The epoch these keys were derived for.
    pub epoch: u16,
    /// The current key and the key that replaces it at the next epoch boundary.
    pub keys: EncryptionKeySet,
    /// Signature over the current key's attestation commitment.
    pub current_attestation: Signature,
    /// Signature over the next key's attestation commitment, absent only when no next key exists.
    pub next_attestation: Option<Signature>,
}

// VALIDATOR SERVICE
// ================================================================================

/// The underlying implementation of the gRPC validator server.
///
/// Implements the gRPC API for the validator.
pub(crate) struct ValidatorService {
    signer: Arc<ValidatorSigner>,
    /// Decrypter for transaction inputs sealed against the shared encryption key.
    decrypter: Arc<dyn TransactionInputDecrypter>,
    /// The attested encryption keys of the epoch currently served. Replaced by the key rotation
    /// task after each epoch boundary.
    encryption_keys: Arc<std::sync::RwLock<Arc<AttestedEncryptionKeys>>>,
    /// Commitment of the genesis block header, binding key attestations to this chain.
    genesis_commitment: Word,
    db: Arc<Database>,
    block_store: BlockStore,
    /// Enforces mutual exclusion between backup block subscriptions and all other RPCs. Regular
    /// RPCs take the read side (any number may run concurrently); a backup subscription takes the
    /// exclusive write side for its entire lifetime. Acquired with `try_*` on both sides so that a
    /// conflicting request fails fast with `resource_exhausted` rather than blocking.
    serve_lock: Arc<tokio::sync::RwLock<()>>,
    /// Serializes `sign_block` requests so that concurrent calls are processed sequentially,
    /// ensuring consistent chain tip reads and preventing race conditions.
    sign_block_semaphore: Semaphore,
    /// In-memory chain tip, updated after each signed block. Block subscriptions follow this to
    /// stream live blocks as they are signed.
    committed_tip: watch::Sender<BlockNumber>,
    /// In-memory count of validated transactions, incremented after each new insert.
    validated_transactions_count: AtomicU64,
    /// In-memory count of signed blocks, incremented after each signed block.
    signed_blocks_count: AtomicU64,
}

impl ValidatorService {
    /// How long the key rotation task waits before retrying a failed rotation, in the absence of
    /// newly signed blocks.
    const KEY_ROTATION_RETRY_DELAY: Duration = Duration::from_secs(30);

    pub(crate) async fn new(
        signer: ValidatorSigner,
        decrypter: Arc<dyn TransactionInputDecrypter>,
        db: Database,
        block_store: BlockStore,
        initial_chain_tip: u32,
        initial_tx_count: u64,
        initial_block_count: u64,
    ) -> Result<Self, ValidatorError> {
        // The validator key is fixed at genesis and carried forward unchanged by every block, so
        // the signing key must match the chain's validator key for this validator's lifetime.
        // Reject a misconfigured key here.
        let chain_tip = db
            .read("load_chain_tip", load_chain_tip)
            .await
            .map_err(ValidatorError::DatabaseError)?
            .ok_or(ValidatorError::NoChainTip)?;
        let signing_key = signer.public_key();
        let expected_key = match chain_tip.validator_keys().as_keys() {
            [key] => key,
            keys => {
                return Err(ValidatorError::UnexpectedValidatorSetSize { actual: keys.len() });
            },
        };
        if &signing_key != expected_key {
            return Err(ValidatorError::ValidatorKeyMismatch {
                expected: expected_key.clone(),
                actual: signing_key,
            });
        }

        // Derive and attest the keys of the current epoch before serving. The key rotation task
        // re-derives and re-signs them after each epoch boundary, so KMS-backed signers see two
        // signing calls per epoch.
        let genesis_commitment = db
            .read("load_genesis_header", |tx| load_block_header(tx, BlockNumber::GENESIS))
            .await
            .map_err(ValidatorError::DatabaseError)?
            .ok_or(ValidatorError::NoGenesisHeader)?
            .commitment();
        let epoch = BlockNumber::from(initial_chain_tip).block_epoch();
        Self::archive_encryption_keys(&db, &decrypter, epoch.saturating_add(1)).await?;
        let encryption_keys =
            Self::attest_encryption_keys(&signer, decrypter.as_ref(), genesis_commitment, epoch)
                .await?;

        Ok(Self {
            signer: Arc::new(signer),
            decrypter,
            encryption_keys: Arc::new(std::sync::RwLock::new(Arc::new(encryption_keys))),
            genesis_commitment,
            serve_lock: Arc::new(tokio::sync::RwLock::new(())),
            db: db.into(),
            block_store,
            sign_block_semaphore: Semaphore::new(1),
            committed_tip: watch::Sender::new(BlockNumber::from(initial_chain_tip)),
            validated_transactions_count: AtomicU64::new(initial_tx_count),
            signed_blocks_count: AtomicU64::new(initial_block_count),
        })
    }

    /// Derives the encryption keys of the given epoch and signs their attestation commitments.
    ///
    /// The current key's commitment is signed with the current-key role suffix, and the next
    /// key's commitment binds its rotation block. See
    /// [`crate::signers::attestation_commitment`].
    async fn attest_encryption_keys(
        signer: &ValidatorSigner,
        decrypter: &dyn TransactionInputDecrypter,
        genesis_commitment: Word,
        epoch: u16,
    ) -> Result<AttestedEncryptionKeys, ValidatorError> {
        let keys = decrypter
            .encryption_keys(epoch)
            .await
            .map_err(|err| ValidatorError::EncryptionKeyAttestationFailed(err.to_string()))?;
        let current_attestation = signer
            .sign_commitment(keys.current.attestation_commitment(genesis_commitment))
            .await
            .map_err(|err| ValidatorError::EncryptionKeyAttestationFailed(err.to_string()))?;
        let next_attestation = match &keys.next {
            Some(next) => Some(
                signer
                    .sign_commitment(next.attestation_commitment(genesis_commitment))
                    .await
                    .map_err(|err| {
                        ValidatorError::EncryptionKeyAttestationFailed(err.to_string())
                    })?,
            ),
            None => None,
        };

        Ok(AttestedEncryptionKeys {
            epoch,
            keys,
            current_attestation,
            next_attestation,
        })
    }

    /// Archives the secret encryption keys of every epoch up to and including `up_to_epoch`.
    ///
    /// Callers pass the epoch FOLLOWING the one being attested: the next key is announced and
    /// attested a whole epoch ahead of its rotation block, so clients may already be sealing
    /// against it and its secret must be archived along with the current one.
    ///
    /// Keys already archived are skipped, so this both backfills epochs missed while the
    /// validator was offline and is a no-op when the archive is up to date. If the decrypter
    /// cannot export secret key bytes (e.g. a TEE-held key), archival is skipped entirely.
    async fn archive_encryption_keys(
        db: &Database,
        decrypter: &Arc<dyn TransactionInputDecrypter>,
        up_to_epoch: u16,
    ) -> Result<(), ValidatorError> {
        let start = db
            .read("max_archived_encryption_key_epoch", max_archived_encryption_key_epoch)
            .await
            .map_err(ValidatorError::DatabaseError)?
            .map_or(0, |max| max.saturating_add(1));

        for epoch in start..=up_to_epoch {
            let secret_key = decrypter
                .export_secret_key(epoch)
                .await
                .map_err(|err| ValidatorError::EncryptionKeyArchivalFailed(err.to_string()))?;
            let Some(secret_key) = secret_key else {
                tracing::debug!(
                    target: COMPONENT,
                    "The decrypter cannot export secret keys, skipping encryption key archival"
                );
                return Ok(());
            };
            let keys = decrypter
                .encryption_keys(epoch)
                .await
                .map_err(|err| ValidatorError::EncryptionKeyArchivalFailed(err.to_string()))?;
            let key = ArchivedEncryptionKey {
                scheme: keys.current.scheme,
                key_id: keys.current.key_id,
                public_key: keys.current.public_key,
                secret_key,
            };
            db.write("insert_encryption_key", move |tx| insert_encryption_key(tx, epoch, &key))
                .await
                .map_err(ValidatorError::DatabaseError)?;
            tracing::info!(
                target: LOG_TARGET,
                epoch,
                "Archived the transaction encryption key"
            );
        }

        Ok(())
    }

    /// Returns the attested encryption keys currently served.
    pub(crate) fn attested_encryption_keys(&self) -> Arc<AttestedEncryptionKeys> {
        self.encryption_keys
            .read()
            .expect("encryption key lock must not be poisoned")
            .clone()
    }

    /// Spawns the key rotation task, which follows the committed chain tip and re-derives and
    /// re-attests the encryption keys after each epoch boundary.
    ///
    /// Signing happens on this task, off the request path, so a slow signer
    /// never delays block signing or key requests. If attestation fails, the previous epoch's
    /// state remains served and the rotation is retried on the next signed block or after
    /// [`Self::KEY_ROTATION_RETRY_DELAY`], whichever comes first.
    pub(crate) fn spawn_key_rotation_task(
        &self,
        shutdown: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let signer = Arc::clone(&self.signer);
        let decrypter = Arc::clone(&self.decrypter);
        let state = Arc::clone(&self.encryption_keys);
        let db = Arc::clone(&self.db);
        let genesis_commitment = self.genesis_commitment;
        let committed_tip = self.committed_tip.subscribe();

        tokio::spawn(async move {
            loop {
                let worker = tokio::spawn(Self::key_rotation_loop(
                    Arc::clone(&signer),
                    Arc::clone(&decrypter),
                    Arc::clone(&state),
                    Arc::clone(&db),
                    genesis_commitment,
                    committed_tip.clone(),
                    Self::KEY_ROTATION_RETRY_DELAY,
                    shutdown.clone(),
                ));
                match worker.await {
                    // The loop exits cleanly only on shutdown or when the tip channel closes.
                    Ok(()) => break,
                    Err(err) => {
                        tracing::error!(
                            target: LOG_TARGET,
                            %err,
                            "The key rotation task terminated abnormally, restarting it"
                        );
                    },
                }
                if shutdown.is_cancelled() {
                    break;
                }
            }
        })
    }

    /// Follows the committed chain tip and re-derives, re-archives, and re-attests the encryption
    /// keys after each epoch boundary. See [`Self::spawn_key_rotation_task`].
    ///
    /// While a rotation is failing, retries are paced by `retry_delay` rather than by every newly
    /// signed block, bounding the extra load on a possibly degraded signer.
    #[expect(clippy::too_many_arguments, reason = "task inputs, spawned detached from &self")]
    async fn key_rotation_loop(
        signer: Arc<ValidatorSigner>,
        decrypter: Arc<dyn TransactionInputDecrypter>,
        state: Arc<std::sync::RwLock<Arc<AttestedEncryptionKeys>>>,
        db: Arc<Database>,
        genesis_commitment: Word,
        mut committed_tip: watch::Receiver<BlockNumber>,
        retry_delay: Duration,
        shutdown: CancellationToken,
    ) {
        let mut retry_pending = false;
        loop {
            let retry_timer_fired = tokio::select! {
                () = shutdown.cancelled() => break,
                changed = committed_tip.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    false
                },
                () = tokio::time::sleep(retry_delay), if retry_pending => true,
            };

            let epoch = committed_tip.borrow_and_update().block_epoch();
            let served_epoch =
                state.read().expect("encryption key lock must not be poisoned").epoch;
            if epoch <= served_epoch {
                retry_pending = false;
                continue;
            }
            if retry_pending && !retry_timer_fired {
                continue;
            }
            if epoch - served_epoch > 1 {
                // The decrypt grace window covers a single epoch, so submissions sealed against a
                // key this stale can become undecryptable.
                tracing::error!(
                    target: LOG_TARGET,
                    epoch,
                    served_epoch,
                    "Serving a transaction encryption key more than one epoch stale"
                );
            }

            // Archive the new epoch's secret key (and its announced next key) before attesting, so
            // a failed archival is retried without spending signatures.
            let archive_up_to = epoch.saturating_add(1);
            if let Err(err) = Self::archive_encryption_keys(&db, &decrypter, archive_up_to).await {
                tracing::warn!(
                    target: LOG_TARGET,
                    epoch,
                    %err,
                    "Failed to archive the rotated transaction encryption key, retrying shortly"
                );
                retry_pending = true;
                continue;
            }

            match Self::attest_encryption_keys(
                &signer,
                decrypter.as_ref(),
                genesis_commitment,
                epoch,
            )
            .await
            {
                Ok(rotated) => {
                    tracing::info!(
                        target: LOG_TARGET,
                        epoch,
                        key_id = %hex::encode(&rotated.keys.current.key_id),
                        "Rotated the transaction encryption key"
                    );
                    *state.write().expect("encryption key lock must not be poisoned") =
                        Arc::new(rotated);
                    retry_pending = false;
                },
                Err(err) => {
                    tracing::warn!(
                        target: LOG_TARGET,
                        epoch,
                        %err,
                        "Failed to attest the rotated transaction encryption key, retrying shortly"
                    );
                    retry_pending = true;
                },
            }
        }
    }

    /// Validates a proposed block by checking:
    /// 1. All transactions have been previously validated by this validator.
    /// 2. The block header can be successfully built from the proposed block.
    /// 3. The block is either: a. The valid next block in the chain (sequential block number, matching
    ///    previous block commitment), or b. A replacement block at the same height as the current chain
    ///    tip, validated against the previous block header.
    ///
    /// On success, returns the signature and the validated block header.
    #[miden_instrument(
        target = COMPONENT,
        skip_all,
        err,
    )]
    pub async fn validate_block(
        &self,
        proposed_block: ProposedBlock,
        chain_tip: BlockHeader,
    ) -> Result<(Signature, BlockHeader), ValidatorError> {
        miden_span_record!(tip.number = chain_tip.block_num().as_u32(),);

        // Search for any proposed transactions that have not previously been validated.
        let proposed_tx_ids =
            proposed_block.transactions().map(TransactionHeader::id).collect::<Vec<_>>();
        let unvalidated_txs = self
            .db
            .read("find_unvalidated_transactions", move |tx| {
                find_unvalidated_transactions(tx, &proposed_tx_ids)
            })
            .await
            .map_err(ValidatorError::DatabaseError)?;

        // All proposed transactions must have been validated.
        if !unvalidated_txs.is_empty() {
            return Err(ValidatorError::UnvalidatedTransactions(unvalidated_txs));
        }

        // Build the block header.
        let (proposed_header, proposed_body) = proposed_block
            .into_header_and_body()
            .map_err(ValidatorError::BlockBuildingFailed)?;

        miden_span_record!(
            block.number = proposed_header.block_num().as_u32(),
            block.commitment = %proposed_header.commitment(),
        );

        // If the proposed block has the same block number as the current chain tip, this is a
        // replacement block. Validate it against the previous block header.
        let prev = if proposed_header.block_num() == chain_tip.block_num() {
            // The genesis block cannot be replaced (genesis block has no parent).
            let prev_block_num =
                chain_tip.block_num().parent().ok_or(ValidatorError::NoPrevBlockHeader)?;
            self.db
                .read("load_block_header", move |tx| load_block_header(tx, prev_block_num))
                .await
                .map_err(ValidatorError::DatabaseError)?
                .ok_or(ValidatorError::NoPrevBlockHeader)?
        } else {
            // Proposed block is a new block. Block number must be sequential.
            let expected_block_num = chain_tip.block_num().child();
            if proposed_header.block_num() != expected_block_num {
                return Err(ValidatorError::BlockNumberMismatch {
                    expected: expected_block_num,
                    actual: proposed_header.block_num(),
                });
            }
            // Current chain tip is the parent of the proposed block.
            chain_tip
        };

        // The proposed block's parent must match the block that the Validator has determined is its
        // parent (either chain tip or parent of chain tip).
        if proposed_header.prev_block_commitment() != prev.commitment() {
            return Err(ValidatorError::PrevBlockCommitmentMismatch);
        }

        // Check that the block's validator key is set to our own.
        //
        // Otherwise we could be signing a block for a different key, making the
        // signature invalid.
        let signing_key = self.signer.public_key();
        let expected_key = match proposed_header.validator_keys().as_keys() {
            [key] => key,
            keys => {
                return Err(ValidatorError::UnexpectedValidatorSetSize { actual: keys.len() });
            },
        };
        if &signing_key != expected_key {
            return Err(ValidatorError::ValidatorKeyMismatch {
                expected: expected_key.clone(),
                actual: signing_key,
            });
        }

        let signature = self.sign_header(&proposed_header).await?;

        // Back up the signed block to disk.
        let signatures = BlockSignatures::new(vec![signature.clone()])
            .map_err(|err| ValidatorError::BlockSigningFailed(err.to_string()))?;
        let signed_block = SignedBlock::new_unchecked(proposed_header, proposed_body, signatures);
        self.block_store
            .save_block(signed_block.header().block_num(), &signed_block.to_bytes())
            .await
            .map_err(ValidatorError::BlockBackupFailed)?;

        let (header, ..) = signed_block.into_parts();
        Ok((signature, header))
    }

    /// Signs a block header using the validator's signer.
    #[miden_instrument(
        target = COMPONENT,
        name = "sign_block",
        skip_all,
        err,
        fields(
            block.number = header.block_num().as_u32(),
        ),
    )]
    async fn sign_header(&self, header: &BlockHeader) -> Result<Signature, ValidatorError> {
        self.signer
            .sign_commitment(header.commitment())
            .await
            .map_err(|err| ValidatorError::BlockSigningFailed(err.to_string()))
    }
}
