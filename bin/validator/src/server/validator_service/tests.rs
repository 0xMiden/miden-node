use std::collections::BTreeMap;

use miden_node_proto::generated::{self as proto};
use miden_node_proto::server::validator_api;
use miden_node_store::{BlockStore, GenesisState};
use miden_node_utils::fee::test_fee_params;
use miden_protocol::block::{BlockHeader, BlockInputs, ProposedBlock};
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::{Signature, SigningKey};
use miden_protocol::crypto::dsa::eddsa_25519_sha512::KeyExchangeKey;
use miden_protocol::testing::random_secret_key::random_secret_key;
use miden_protocol::transaction::PartialBlockchain;
use miden_protocol::{Hasher, Word};
use miden_tx::utils::serde::{Deserializable, Serializable};

use super::{ValidatorError, ValidatorService};
use crate::db::{load_chain_tip, setup, upsert_block_header};
use crate::{ValidatorEncryptor, ValidatorSigner};

// TEST HELPERS
// ================================================================================================

/// The shared transaction encryption secret provisioned to every test validator.
const TEST_ENCRYPTION_SECRET: [u8; 32] = [3u8; 32];

/// Creates a [`ValidatorEncryptor`] from the shared test secret, modelling the identically
/// provisioned encryption key of a validator in the set.
fn test_encryptor() -> ValidatorEncryptor {
    let key = KeyExchangeKey::read_from_bytes(&TEST_ENCRYPTION_SECRET)
        .expect("test secret should be a valid key exchange key");
    ValidatorEncryptor::new_local(key)
}

/// Test harness that wraps a [`Validator`] and tracks the chain MMR state needed to construct valid
/// [`ProposedBlock`]s.
struct TestValidator {
    server: ValidatorService,
    chain: PartialBlockchain,
    chain_tip: BlockHeader,
    // Keeps the database's temp directory alive for the validator's lifetime: the reader pool opens
    // connections lazily, so the file must still exist when the first read runs.
    _temp_dir: tempfile::TempDir,
}

impl TestValidator {
    /// Creates a correctly configured [`ValidatorService`]: the validator signs blocks with the
    /// same key that is designated as the `validator_key` in the genesis block.
    async fn new() -> Self {
        let key = random_secret_key();
        let signer = ValidatorSigner::new_local(key.clone());
        let (temp_dir, db, block_store, genesis_header) = setup_db_with_genesis(&key).await;

        Self {
            server: ValidatorService::new(signer, test_encryptor(), db, block_store, 0, 0, 0)
                .await
                .unwrap(),
            chain: PartialBlockchain::default(),
            chain_tip: genesis_header,
            _temp_dir: temp_dir,
        }
    }

    /// Builds an empty [`ProposedBlock`] extending the current chain tip.
    fn propose_empty_block(&self) -> ProposedBlock {
        empty_block(&self.chain_tip, &self.chain)
    }

    /// Calls `sign_block` on the validator server.
    async fn call_sign_block(
        &self,
        proposed_block: &ProposedBlock,
    ) -> Result<proto::blockchain::SignBlockResponse, tonic::Status> {
        let request = tonic::Request::new(proto::blockchain::ProposedBlock {
            proposed_block: proposed_block.to_bytes(),
        });
        validator_api::SignBlock::full(&self.server, request).await
    }

    /// Opens a block subscription starting from `block_from`.
    async fn call_block_subscription(
        &self,
        block_from: u32,
    ) -> <ValidatorService as proto::server::validator_api::BlockSubscription>::ItemStream {
        self.try_call_block_subscription(block_from)
            .await
            .expect("subscription should open")
    }

    /// Opens a block subscription starting from `block_from`, returning the raw result so callers
    /// can assert on rejection.
    async fn try_call_block_subscription(
        &self,
        block_from: u32,
    ) -> Result<
        <ValidatorService as proto::server::validator_api::BlockSubscription>::ItemStream,
        tonic::Status,
    > {
        let request =
            tonic::Request::new(proto::validator::BlockSubscriptionRequest { block_from });
        validator_api::BlockSubscription::full(&self.server, request).await
    }

    /// Calls the `status` endpoint on the validator server.
    async fn call_status(&self) -> proto::validator::ValidatorStatus {
        validator_api::Status::full(&self.server, tonic::Request::new(()))
            .await
            .expect("status should always be available")
    }

    /// Calls the `get_transaction_encryption_key` endpoint on the validator server.
    async fn call_get_transaction_encryption_key(
        &self,
    ) -> proto::transaction::TransactionEncryptionKey {
        validator_api::GetTransactionEncryptionKey::full(&self.server, tonic::Request::new(()))
            .await
            .expect("encryption key should always be available")
    }

    /// Asserts that opening a backup subscription is rejected with `resource_exhausted`. The
    /// success type ([`Self::ItemStream`]) is not `Debug`, so we match rather than `expect_err`.
    async fn assert_backup_rejected(&self, block_from: u32) {
        match self.try_call_block_subscription(block_from).await {
            Ok(_) => panic!("backup subscription should have been rejected"),
            Err(status) => {
                assert_eq!(status.code(), tonic::Code::ResourceExhausted, "got: {status:?}");
            },
        }
    }

    /// Loads the current chain tip from the validator's database.
    async fn load_chain_tip(&self) -> BlockHeader {
        self.server
            .db
            .read("load_chain_tip", load_chain_tip)
            .await
            .unwrap()
            .expect("chain tip should exist")
    }

    /// Builds, submits, and applies an empty block, advancing the chain tip.
    ///
    /// Panics if the block is rejected.
    async fn apply_empty_block(&mut self) {
        let proposed = self.propose_empty_block();
        self.call_sign_block(&proposed).await.unwrap();
        let (header, _) = proposed.into_header_and_body().unwrap();
        // Advance our local chain state to match what the server now has.
        self.chain.add_block(&self.chain_tip, false);
        self.chain_tip = header;
    }
}

/// Creates a validator database seeded with a genesis block whose `validator_key` is the public key
/// of `key`. Returns the database handle and the genesis block header.
async fn setup_db_with_genesis(
    key: &SigningKey,
) -> (tempfile::TempDir, miden_node_db::sqlite::Database, BlockStore, BlockHeader) {
    let genesis_state = GenesisState::new(vec![], test_fee_params(), 1, 0, key.public_key());
    let genesis_block = genesis_state.into_block(key).unwrap();
    let genesis_header = genesis_block.inner().header().clone();

    let dir = tempfile::tempdir().unwrap();
    let db = setup(dir.path().join("validator.sqlite3")).await.unwrap();
    let block_store =
        BlockStore::bootstrap(dir.path().join("blocks").clone(), &genesis_block).unwrap();

    db.write("upsert_genesis", {
        let h = genesis_header.clone();
        move |tx| upsert_block_header(tx, &h)
    })
    .await
    .unwrap();

    (dir, db, block_store, genesis_header)
}

/// Builds an empty [`ProposedBlock`] that extends the given parent block header using the provided
/// partial blockchain state.
fn empty_block(parent_header: &BlockHeader, chain: &PartialBlockchain) -> ProposedBlock {
    let block_inputs = BlockInputs::new(
        parent_header.clone(),
        chain.clone(),
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
    );
    ProposedBlock::new(block_inputs, vec![]).unwrap()
}

// TESTS
// ================================================================================================

/// A validator whose signing key does not match the `validator_key` designated by the chain
/// (carried forward from genesis) must fail to start, rather than coming up and silently producing
/// signatures that the block producer cannot verify.
#[tokio::test]
async fn signing_key_mismatch_rejected() {
    // Seed a database whose genesis designates `genesis_key` as the validator key.
    let genesis_key = random_secret_key();
    let (_temp_dir, db, block_store, genesis_header) = setup_db_with_genesis(&genesis_key).await;

    // Start a validator with a different key, modelling a validator configured with the wrong key.
    let rogue_signer = ValidatorSigner::new_local(random_secret_key());
    assert_ne!(
        [rogue_signer.public_key()].as_slice(),
        genesis_header.validator_keys().as_keys(),
        "test requires a signing key that differs from the genesis validator key",
    );

    let result =
        ValidatorService::new(rogue_signer, test_encryptor(), db, block_store, 0, 0, 0).await;
    assert!(
        matches!(result, Err(ValidatorError::ValidatorKeyMismatch { .. })),
        "expected ValidatorKeyMismatch error",
    );
}

/// The `SignBlock` response reports the commitment of the block the validator signed, and it
/// matches the commitment the caller derives from the same proposed block. This lets the block
/// producer detect a block-hash mismatch between itself and the validator.
#[tokio::test]
async fn sign_block_returns_signed_commitment() {
    let tv = TestValidator::new().await;

    let proposed = tv.propose_empty_block();
    let response = tv.call_sign_block(&proposed).await.expect("block should be signed");

    let (header, _) = proposed.into_header_and_body().unwrap();
    let returned: Word = response
        .block_commitment
        .expect("response should carry the signed commitment")
        .try_into()
        .unwrap();
    assert_eq!(
        returned,
        header.commitment(),
        "returned commitment must match the proposed block's commitment",
    );
}

/// An empty block at chain tip + 1 with the correct previous block commitment should be accepted.
#[tokio::test]
async fn chain_tip_plus_one_succeeds() {
    let tv = TestValidator::new().await;

    let proposed = tv.propose_empty_block();
    let result = tv.call_sign_block(&proposed).await;

    assert!(result.is_ok(), "chain tip + 1 should succeed, got: {:?}", result.err());
}

/// A replacement block at the same height as the current chain tip should be accepted.
#[tokio::test]
async fn chain_tip_replacement_succeeds() {
    let mut tv = TestValidator::new().await;

    // The genesis block can never be replaced, so we advance the chain to block 1, which we can
    // then replace.
    let genesis_header = tv.chain_tip.clone();
    let chain_at_genesis = tv.chain.clone();
    tv.apply_empty_block().await;
    let original_header = tv.chain_tip.clone();

    // Submit a different block at the same height (block 1), which is a replacement. Use an
    // explicit timestamp far in the future to ensure the replacement block differs.
    let block_inputs = BlockInputs::new(
        genesis_header.clone(),
        chain_at_genesis.clone(),
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
    );
    let far_future_timestamp = genesis_header.timestamp() + 1_000_000;
    let replacement = ProposedBlock::new_at(block_inputs, vec![], far_future_timestamp).unwrap();
    let (replacement_header, _) = replacement.clone().into_header_and_body().unwrap();

    assert_eq!(replacement_header.block_num(), original_header.block_num());
    assert_ne!(
        replacement_header.commitment(),
        original_header.commitment(),
        "replacement block should differ from the original"
    );

    let result = tv.call_sign_block(&replacement).await;
    assert!(result.is_ok(), "chain tip replacement should succeed, got: {:?}", result.err());

    // Verify that the chain tip in the database is now the replacement block, not the original.
    let new_chain_tip = tv.load_chain_tip().await;
    assert_eq!(
        new_chain_tip.commitment(),
        replacement_header.commitment(),
        "chain tip should be the replacement block"
    );
    assert_ne!(
        new_chain_tip.commitment(),
        original_header.commitment(),
        "chain tip should no longer be the original block"
    );
}

/// A block at chain tip + 2 (skipping a block number) should be rejected.
#[tokio::test]
async fn chain_tip_plus_two_rejected() {
    let mut tv = TestValidator::new().await;

    // Apply block 1.
    tv.apply_empty_block().await;

    // Build block 2 locally without applying it, then build block 3 on top.
    let block_2 = tv.propose_empty_block();
    let (block_2_header, _) = block_2.into_header_and_body().unwrap();
    let mut chain_after_1 = tv.chain.clone();
    chain_after_1.add_block(&tv.chain_tip, false);
    let block_3 = empty_block(&block_2_header, &chain_after_1);

    let result = tv.call_sign_block(&block_3).await;
    assert!(result.is_err(), "chain tip + 2 should be rejected");
    let status = result.unwrap_err();
    assert!(
        status.message().contains("block number mismatch"),
        "expected block number mismatch error, got: {}",
        status.message()
    );
}

/// A block at chain tip - 1 (behind the tip) should be rejected.
#[tokio::test]
async fn chain_tip_minus_one_rejected() {
    let mut tv = TestValidator::new().await;

    // Save genesis state.
    let genesis_header = tv.chain_tip.clone();
    let chain_at_genesis = tv.chain.clone();

    // Advance the chain to block 2.
    tv.apply_empty_block().await;
    tv.apply_empty_block().await;

    // Try to submit a block at height 1 (chain tip - 1). This is neither a replacement (which would
    // need to match tip height 2) nor the next block (which would be 3).
    let stale_block = empty_block(&genesis_header, &chain_at_genesis);

    let result = tv.call_sign_block(&stale_block).await;
    assert!(result.is_err(), "chain tip - 1 should be rejected");
    let status = result.unwrap_err();
    assert!(
        status.message().contains("block number mismatch"),
        "expected block number mismatch error, got: {}",
        status.message()
    );
}

/// A block with the wrong previous block commitment should be rejected.
#[tokio::test]
async fn commitment_mismatch_rejected() {
    let tv = TestValidator::new().await;

    // Build a valid ProposedBlock on a *different* genesis so its prev_block_commitment won't match
    // the validator's actual chain tip.
    let other_genesis_signer = random_secret_key();
    let other_genesis_state =
        GenesisState::new(vec![], test_fee_params(), 1, 1, other_genesis_signer.public_key());
    let other_genesis_block = other_genesis_state.into_block(&other_genesis_signer).unwrap();
    let other_genesis_header = other_genesis_block.inner().header().clone();
    let mismatched_block = empty_block(&other_genesis_header, &PartialBlockchain::default());

    let result = tv.call_sign_block(&mismatched_block).await;
    assert!(result.is_err(), "commitment mismatch should be rejected");
    let status = result.unwrap_err();
    assert!(
        status.message().contains("previous block commitment"),
        "expected commitment mismatch error, got: {}",
        status.message()
    );
}

/// A replacement block (same height as chain tip) with the wrong parent commitment should be
/// rejected.
#[tokio::test]
async fn replacement_commitment_mismatch_rejected() {
    let mut tv = TestValidator::new().await;

    // Advance past genesis so we have a replaceable block.
    tv.apply_empty_block().await;

    // Build a replacement block at the same height but using a *different* genesis so its
    // prev_block_commitment won't match the validator's actual parent of the chain tip.
    let other_genesis_signer = random_secret_key();
    let other_genesis_state =
        GenesisState::new(vec![], test_fee_params(), 1, 1, other_genesis_signer.public_key());
    let other_genesis_block = other_genesis_state.into_block(&other_genesis_signer).unwrap();
    let other_genesis_header = other_genesis_block.inner().header().clone();
    let mismatched_replacement = empty_block(&other_genesis_header, &PartialBlockchain::default());

    let result = tv.call_sign_block(&mismatched_replacement).await;
    assert!(result.is_err(), "replacement with mismatched commitment should be rejected");
    let status = result.unwrap_err();
    assert!(
        status.message().contains("previous block commitment"),
        "expected commitment mismatch error, got: {}",
        status.message()
    );
}

/// An empty block (no transactions, no batches) should be accepted.
#[tokio::test]
async fn empty_block_succeeds() {
    let tv = TestValidator::new().await;

    let proposed = tv.propose_empty_block();
    assert_eq!(proposed.transactions().count(), 0, "block should have no transactions");

    let result = tv.call_sign_block(&proposed).await;
    assert!(result.is_ok(), "empty block should succeed, got: {:?}", result.err());
}

/// A block containing transactions that were not previously validated should be rejected.
#[tokio::test]
async fn unknown_transactions_rejected() {
    use miden_protocol::Word;
    use miden_protocol::batch::{BatchAccountUpdate, BatchId, ProvenBatch};
    use miden_protocol::block::BlockNumber;
    use miden_protocol::testing::account_id::ACCOUNT_ID_SENDER;
    use miden_protocol::transaction::{
        InputNoteCommitment,
        InputNotes,
        OrderedTransactionHeaders,
        TransactionHeader,
    };
    use miden_protocol::vm::ExecutionProof;

    let tv = TestValidator::new().await;
    let genesis_header = tv.chain_tip.clone();

    // Build a dummy transaction header with a transaction ID that has NOT been submitted through
    // `submit_proven_transaction`.
    let account_id = ACCOUNT_ID_SENDER.try_into().unwrap();
    let tx_header = TransactionHeader::new(
        account_id,
        Word::default(),
        Word::default(),
        InputNotes::<InputNoteCommitment>::default(),
        vec![],
    );
    let tx_id = tx_header.id();

    // Build a ProvenBatch containing this transaction.
    let batch = ProvenBatch::new_unchecked(
        BatchId::from_ids(std::iter::once((tx_id, account_id))),
        genesis_header.commitment(),
        BlockNumber::GENESIS,
        BTreeMap::from([(
            account_id,
            BatchAccountUpdate::new_unchecked(
                account_id,
                Word::default(),
                Word::default(),
                miden_protocol::account::AccountUpdateDetails::Private,
            ),
        )]),
        InputNotes::default(),
        vec![],
        BlockNumber::MAX,
        OrderedTransactionHeaders::new_unchecked(vec![tx_header]),
        ExecutionProof::new_dummy(),
    )
    .unwrap();

    // Build a ProposedBlock containing the batch with the unknown transaction.
    let block_inputs = BlockInputs::new(
        genesis_header.clone(),
        PartialBlockchain::default(),
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
    );
    let proposed = ProposedBlock::new(block_inputs, vec![batch]).unwrap();

    let result = tv.server.validate_block(proposed, genesis_header).await;
    assert!(result.is_err(), "block with unknown transactions should be rejected");
    match result.unwrap_err() {
        ValidatorError::UnvalidatedTransactions(ids) => {
            assert_eq!(ids, vec![tx_id], "should report the unknown transaction ID");
        },
        other => panic!("expected UnvalidatedTransactions error, got: {other}"),
    }
}

/// After replacing the chain tip, a new block built against the pre-replacement tip should be
/// rejected because its previous block commitment no longer matches.
#[tokio::test]
async fn new_block_after_replacement_with_stale_commitment_rejected() {
    let mut tv = TestValidator::new().await;

    // Advance to block 1 and save the state needed to build on top of it.
    let genesis_header = tv.chain_tip.clone();
    let chain_at_genesis = tv.chain.clone();
    tv.apply_empty_block().await;
    let original_block_1_header = tv.chain_tip.clone();
    let chain_after_block_1 = tv.chain.clone();

    // Replace block 1 with a different block at the same height.
    let block_inputs = BlockInputs::new(
        genesis_header.clone(),
        chain_at_genesis.clone(),
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
    );
    let far_future_timestamp = genesis_header.timestamp() + 1_000_000;
    let replacement = ProposedBlock::new_at(block_inputs, vec![], far_future_timestamp).unwrap();
    let (replacement_header, _) = replacement.clone().into_header_and_body().unwrap();
    assert_ne!(
        replacement_header.commitment(),
        original_block_1_header.commitment(),
        "replacement block should differ from the original"
    );
    tv.call_sign_block(&replacement).await.unwrap();

    // Now try to submit block 2 built on top of the *original* block 1. Its prev_block_commitment
    // points to the old block 1, not the replacement.
    let stale_block_2 = empty_block(&original_block_1_header, &chain_after_block_1);

    let result = tv.call_sign_block(&stale_block_2).await;
    assert!(
        result.is_err(),
        "block with stale commitment after replacement should be rejected"
    );
    let status = result.unwrap_err();
    assert!(
        status.message().contains("previous block commitment"),
        "expected commitment mismatch error, got: {}",
        status.message()
    );
}

/// Verify that `validate_block` rejects blocks with a non-sequential block number.
#[tokio::test]
async fn validate_block_number_mismatch() {
    let mut tv = TestValidator::new().await;

    // Advance to block 1.
    tv.apply_empty_block().await;
    let block_1_header = tv.chain_tip.clone();

    // Build block 2 and 3 locally, then try to submit block 3 with chain_tip = block 1.
    let mut chain = tv.chain.clone();
    let block_2 = empty_block(&block_1_header, &chain);
    let (block_2_header, _) = block_2.into_header_and_body().unwrap();

    chain.add_block(&block_1_header, false);
    let block_3 = empty_block(&block_2_header, &chain);

    let result = tv.server.validate_block(block_3, block_1_header).await;
    assert!(result.is_err());
    assert!(
        matches!(result.unwrap_err(), ValidatorError::BlockNumberMismatch { .. }),
        "expected BlockNumberMismatch error"
    );
}

/// A block subscription replays the backed-up blocks from the requested height. While the
/// subscription is live it holds the exclusive backup lock, so signing is frozen for its duration
/// and no further blocks can be produced or streamed.
#[tokio::test]
async fn block_subscription_replays_then_freezes_signing() {
    use std::time::Duration;

    use miden_protocol::block::SignedBlock;
    use miden_tx::utils::serde::Deserializable;
    use tokio_stream::StreamExt;

    let mut tv = TestValidator::new().await;

    // Sign blocks 1 and 2 so the validator backs them up to its block store.
    tv.apply_empty_block().await;
    tv.apply_empty_block().await;

    // Subscribe from the first signed block and confirm the backed-up blocks are replayed in order.
    let mut stream = tv.call_block_subscription(1).await;
    for expected in 1..=2 {
        let response = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("replayed block should arrive promptly")
            .expect("stream should not end")
            .expect("stream item should not be an error");
        let block = SignedBlock::read_from_bytes(&response.block).expect("valid signed block");
        assert_eq!(block.header().block_num().as_u32(), expected);
        assert_eq!(response.committed_chain_tip, 2);
    }

    // The live subscription holds the backup lock, so no new block can be signed while it is open.
    // The validator therefore cannot produce a block to stream, and signing is rejected until the
    // subscriber disconnects.
    let proposed = tv.propose_empty_block();
    let status = tv
        .call_sign_block(&proposed)
        .await
        .expect_err("sign_block must be rejected while a backup subscription is live");
    assert_eq!(status.code(), tonic::Code::ResourceExhausted, "got: {status:?}");

    // Once the subscriber disconnects, signing resumes.
    drop(stream);
    tv.call_sign_block(&proposed)
        .await
        .expect("sign_block should succeed once the subscription is dropped");
}

// SERVE LOCK TESTS
// ================================================================================================
//
// A backup subscription holds the exclusive write side of `serve_lock` for the lifetime of the
// returned stream; every other RPC takes the read side. The two are therefore mutually exclusive:
// a backup cannot start while requests are in flight, and requests are rejected while a backup is
// streaming. Both sides fail fast with `resource_exhausted` rather than blocking.

/// While a backup subscription is streaming, `sign_block` is rejected, and it succeeds again once
/// the subscription is dropped and the lock released.
#[tokio::test]
async fn backup_stream_blocks_sign_block_until_dropped() {
    let mut tv = TestValidator::new().await;
    tv.apply_empty_block().await;

    // Open a backup subscription; the returned stream holds the exclusive lock.
    let stream = tv.call_block_subscription(1).await;

    let proposed = tv.propose_empty_block();
    let status = tv
        .call_sign_block(&proposed)
        .await
        .expect_err("sign_block must be rejected while a backup is streaming");
    assert_eq!(status.code(), tonic::Code::ResourceExhausted, "got: {status:?}");

    // Dropping the subscription releases the lock, so the same request now succeeds.
    drop(stream);
    tv.call_sign_block(&proposed)
        .await
        .expect("sign_block should succeed once the backup stream is dropped");
}

/// Unlike other RPCs, `status` stays available during a backup and reports `BACKUP` instead of
/// `OK`, reverting to `OK` once the subscription is dropped.
#[tokio::test]
async fn status_reports_backup_while_streaming() {
    let mut tv = TestValidator::new().await;
    tv.apply_empty_block().await;

    assert_eq!(tv.call_status().await.status, "OK");

    let stream = tv.call_block_subscription(1).await;
    assert_eq!(
        tv.call_status().await.status,
        "BACKUP",
        "status must report BACKUP while a backup is streaming",
    );

    drop(stream);
    assert_eq!(
        tv.call_status().await.status,
        "OK",
        "status must revert to OK once the backup stream is dropped",
    );
}

/// A backup subscription cannot start while another request holds the read side of the lock,
/// modelling an in-flight RPC. Once that reader is released, the backup opens successfully.
#[tokio::test]
async fn in_flight_request_blocks_backup() {
    let tv = TestValidator::new().await;

    // Simulate an in-flight RPC by holding the read side of the lock, exactly as the RPC handlers
    // do for their duration.
    let read_guard = tv.server.serve_lock.try_read().expect("read side should be available");

    tv.assert_backup_rejected(0).await;

    // Releasing the reader lets a backup start.
    drop(read_guard);
    let _stream = tv.call_block_subscription(0).await;
}

/// Only one backup subscription can run at a time: opening a second while the first is live is
/// rejected.
#[tokio::test]
async fn concurrent_backups_rejected() {
    let tv = TestValidator::new().await;

    let first = tv.call_block_subscription(0).await;

    tv.assert_backup_rejected(0).await;

    // The slot frees up once the first subscription is dropped.
    drop(first);
    let _stream = tv.call_block_subscription(0).await;
}

/// Ordinary requests share the read side of the lock and so run concurrently with one another; only
/// a backup is exclusive.
#[tokio::test]
async fn requests_run_concurrently() {
    let tv = TestValidator::new().await;

    // Multiple readers may hold the lock at once, so requests are not serialized against each
    // other.
    let first = tv.server.serve_lock.try_read().expect("first reader should acquire");
    let second = tv
        .server
        .serve_lock
        .try_read()
        .expect("second reader should acquire concurrently");

    // A backup is still excluded while any reader is held.
    tv.assert_backup_rejected(0).await;

    drop(first);
    drop(second);
}

// TRANSACTION ENCRYPTION KEY
// ================================================================================================

/// Recomputes the attestation commitment from response fields and the chain's genesis commitment.
fn attestation_commitment_of(
    scheme: u32,
    key_id: u32,
    genesis_commitment: Word,
    public_key: &[u8],
) -> Word {
    let genesis_commitment = genesis_commitment.to_bytes();
    let mut payload = Vec::with_capacity(
        ValidatorEncryptor::ATTESTATION_DOMAIN.len()
            + 2 * size_of::<u32>()
            + genesis_commitment.len()
            + public_key.len(),
    );
    payload.extend_from_slice(ValidatorEncryptor::ATTESTATION_DOMAIN);
    payload.extend_from_slice(&scheme.to_le_bytes());
    payload.extend_from_slice(&key_id.to_le_bytes());
    payload.extend_from_slice(&genesis_commitment);
    payload.extend_from_slice(public_key);
    Hasher::hash(&payload)
}

/// The endpoint returns the shared encryption key attested by this validator's own signing key. The
/// signature verifies over a commitment recomputed from the response fields and the chain's genesis
/// commitment, so a client needs nothing beyond the response and the chain data it already trusts.
#[tokio::test]
async fn transaction_encryption_key_is_attested() {
    let tv = TestValidator::new().await;
    // The chain has not advanced, so the chain tip is the genesis header.
    let genesis = tv.chain_tip.commitment();
    let response = tv.call_get_transaction_encryption_key().await;

    let encryptor = test_encryptor();
    assert_eq!(response.scheme, u32::from(u8::from(ValidatorEncryptor::SCHEME)));
    assert_eq!(response.key_id, encryptor.key_id());
    assert_eq!(response.public_key, encryptor.public_key().to_bytes());

    let commitment =
        attestation_commitment_of(response.scheme, response.key_id, genesis, &response.public_key);
    assert_eq!(commitment, encryptor.attestation_commitment(genesis));

    let signature =
        Signature::read_from_bytes(&response.signature).expect("signature should deserialize");
    assert!(
        signature.verify(commitment, &tv.server.signer.public_key()),
        "attestation must verify against this validator's signing key",
    );
}

/// Two validators provisioned with the same shared encryption secret but distinct signing keys
/// return identical public key material with different signatures.
#[tokio::test]
async fn shared_key_is_attested_per_validator() {
    let tv_a = TestValidator::new().await;
    let tv_b = TestValidator::new().await;

    let response_a = tv_a.call_get_transaction_encryption_key().await;
    let response_b = tv_b.call_get_transaction_encryption_key().await;

    assert_eq!(response_a.scheme, response_b.scheme);
    assert_eq!(response_a.key_id, response_b.key_id);
    assert_eq!(response_a.public_key, response_b.public_key);
    assert_ne!(
        response_a.signature, response_b.signature,
        "each validator must attest with its own signing key",
    );
}

/// The attestation signature must not survive tampering with any field of the response, nor a
/// swapped chain.
#[tokio::test]
async fn tampered_attestation_fails_verification() {
    let tv = TestValidator::new().await;
    let genesis = tv.chain_tip.commitment();
    let response = tv.call_get_transaction_encryption_key().await;
    let signature = Signature::read_from_bytes(&response.signature).unwrap();
    let signing_key = tv.server.signer.public_key();

    let mut tampered_public_key = response.public_key.clone();
    tampered_public_key[0] ^= 0x01;
    let tampered_genesis = Word::try_from([9u64, 9, 9, 9]).unwrap();

    let tampered_commitments = [
        attestation_commitment_of(
            response.scheme + 1,
            response.key_id,
            genesis,
            &response.public_key,
        ),
        attestation_commitment_of(
            response.scheme,
            response.key_id.wrapping_add(1),
            genesis,
            &response.public_key,
        ),
        attestation_commitment_of(response.scheme, response.key_id, genesis, &tampered_public_key),
        attestation_commitment_of(
            response.scheme,
            response.key_id,
            tampered_genesis,
            &response.public_key,
        ),
    ];
    for commitment in tampered_commitments {
        assert!(
            !signature.verify(commitment, &signing_key),
            "attestation must not verify over tampered fields",
        );
    }
}

/// A client can reconstruct the sealing key from the response fields and seal a payload that any
/// validator holding the shared secret can unseal. Unsealing must reject mismatched associated
/// data.
#[tokio::test]
async fn response_key_seals_for_the_validator_set() {
    use miden_protocol::crypto::dsa::eddsa_25519_sha512::PublicKey as EncryptionPublicKey;
    use miden_protocol::crypto::ies::SealingKey;

    let tv = TestValidator::new().await;
    let response = tv.call_get_transaction_encryption_key().await;

    let public_key = EncryptionPublicKey::read_from_bytes(&response.public_key)
        .expect("response public key should deserialize");
    let sealing_key = SealingKey::X25519XChaCha20Poly1305(public_key);

    let mut rng = rand::rng();
    let plaintext = b"transaction inputs";
    let associated_data = b"scheme|key_id|chain|tx";
    let sealed = sealing_key
        .seal_bytes_with_associated_data(&mut rng, plaintext, associated_data)
        .unwrap();

    let opened = test_encryptor()
        .unseal_bytes_with_associated_data(sealed.clone(), associated_data)
        .unwrap();
    assert_eq!(opened.as_slice(), plaintext);

    assert!(
        test_encryptor()
            .unseal_bytes_with_associated_data(sealed, b"other associated data")
            .is_err(),
        "unsealing must fail under mismatched associated data",
    );
}

/// Like `status`, the encryption key stays available while a backup subscription holds the
/// exclusive serve lock.
#[tokio::test]
async fn encryption_key_available_during_backup() {
    let mut tv = TestValidator::new().await;
    tv.apply_empty_block().await;

    let stream = tv.call_block_subscription(1).await;

    // `call_get_transaction_encryption_key` panics on rejection, so completing proves availability
    // during the backup.
    let response = tv.call_get_transaction_encryption_key().await;
    assert!(!response.public_key.is_empty());

    drop(stream);
}
