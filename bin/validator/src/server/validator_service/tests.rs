use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use miden_node_proto::domain::encryption::{
    TransactionEncryptionScheme,
    TrustedTransactionEncryptionState,
    transaction_inputs_associated_data,
    verify_transaction_encryption_key_schedule,
};
use miden_node_proto::generated::{self as proto};
use miden_node_proto::server::validator_api;
use miden_node_store::{BlockStore, GenesisState};
use miden_node_utils::fee::test_fee_params;
use miden_protocol::Word;
use miden_protocol::account::AccountUpdateDetails;
use miden_protocol::account::auth::AuthScheme;
use miden_protocol::asset::{Asset, FungibleAsset};
use miden_protocol::block::{BlockHeader, BlockInputs, BlockNumber, ProposedBlock, ValidatorKeys};
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey;
use miden_protocol::crypto::dsa::eddsa_25519_sha512::KeyExchangeKey;
use miden_protocol::note::NoteType;
use miden_protocol::testing::account_id::{ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET, ACCOUNT_ID_SENDER};
use miden_protocol::testing::random_secret_key::random_secret_key;
use miden_protocol::transaction::{
    InputNoteCommitment,
    OutputNote,
    PartialBlockchain,
    ProvenTransaction,
    TransactionId,
    TransactionInputs,
    TxAccountUpdate,
};
use miden_protocol::vm::ExecutionProof;
use miden_testing::{Auth, MockChainBuilder};
use miden_tx::LocalTransactionProver;
use miden_tx::utils::serde::{Deserializable, Serializable};
use tokio::sync::OnceCell;

use super::{InitialMetrics, ValidatorError, ValidatorService};
use crate::db::{
    count_validated_transactions,
    load_chain_tip,
    load_private_record,
    setup,
    transaction_exists,
    upsert_block_header,
};
use crate::storage_key::tests::operator_keys;
use crate::{
    LocalX25519TransactionInputDecrypter,
    PrivateRecordSealer,
    TransactionInputDecrypter,
    ValidatorSigner,
};

// TEST HELPERS
// ================================================================================================

/// The shared transaction encryption secret provisioned to every test validator.
const TEST_ENCRYPTION_SECRET: [u8; 32] = [3u8; 32];

/// Creates a [`LocalX25519TransactionInputDecrypter`] from the shared test secret, modelling the
/// identically provisioned encryption key of a validator in the set.
fn test_decrypter() -> LocalX25519TransactionInputDecrypter {
    LocalX25519TransactionInputDecrypter::new(
        KeyExchangeKey::read_from_bytes(&TEST_ENCRYPTION_SECRET).unwrap(),
    )
}

struct FailingScheduleProvider {
    inner: LocalX25519TransactionInputDecrypter,
    schedule_calls: AtomicUsize,
    fail_schedule: AtomicBool,
    panic_schedule: AtomicBool,
    block_schedule: AtomicBool,
    schedule_started: tokio::sync::Notify,
    schedule_released: tokio::sync::Notify,
}

impl FailingScheduleProvider {
    fn new() -> Self {
        Self {
            inner: test_decrypter(),
            schedule_calls: AtomicUsize::new(0),
            fail_schedule: AtomicBool::new(false),
            panic_schedule: AtomicBool::new(false),
            block_schedule: AtomicBool::new(false),
            schedule_started: tokio::sync::Notify::new(),
            schedule_released: tokio::sync::Notify::new(),
        }
    }
}

#[tonic::async_trait]
impl TransactionInputDecrypter for FailingScheduleProvider {
    async fn encryption_key_schedule(
        &self,
        chain_tip: BlockNumber,
    ) -> anyhow::Result<crate::TransactionEncryptionKeySchedule> {
        self.schedule_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_schedule.load(Ordering::SeqCst) {
            anyhow::bail!("schedule provider unavailable");
        }
        assert!(!self.panic_schedule.load(Ordering::SeqCst), "schedule provider panicked");
        if self.block_schedule.load(Ordering::SeqCst) {
            self.schedule_started.notify_one();
            self.schedule_released.notified().await;
        }
        self.inner.encryption_key_schedule(chain_tip).await
    }

    async fn decrypt_transaction_inputs(
        &self,
        key_id: &[u8],
        chain_tip: BlockNumber,
        ciphertext: &[u8],
        associated_data: &[u8],
    ) -> Result<Vec<u8>, crate::TransactionInputDecryptionError> {
        self.inner
            .decrypt_transaction_inputs(key_id, chain_tip, ciphertext, associated_data)
            .await
    }
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
        Self::new_with_decrypter(Arc::new(test_decrypter())).await
    }

    async fn new_with_decrypter(decrypter: Arc<dyn TransactionInputDecrypter>) -> Self {
        let key = random_secret_key();
        let signer = ValidatorSigner::new_local(key.clone());
        let (temp_dir, db, block_store, genesis_header) = setup_db_with_genesis(&key).await;

        Self {
            server: ValidatorService::new(
                signer,
                decrypter,
                PrivateRecordSealer::from_operator_key(&operator_keys().remove(0)),
                db,
                block_store,
                InitialMetrics::new(0, 0, 0),
            )
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

    /// Calls `submit_proven_transaction` on the validator server.
    async fn call_submit_proven_transaction(
        &self,
        tx: &ProvenTransaction,
        sealed: proto::transaction::SealedTransactionInputs,
    ) -> Result<(), tonic::Status> {
        let request = tonic::Request::new(proto::transaction::ProvenTransaction {
            transaction: tx.to_bytes(),
            sealed_transaction_inputs: Some(sealed),
        });
        validator_api::SubmitProvenTransaction::full(&self.server, request).await
    }

    /// Returns the opaque id of the key this validator currently serves.
    async fn current_key_id(&self) -> Vec<u8> {
        self.server
            .attested_encryption_key_schedule()
            .await
            .expect("the test schedule should attest")
            .schedule
            .current_key
            .key_id
            .clone()
    }

    /// Seals `plaintext` exactly as a well-behaved client would: against the key this validator
    /// serves, bound to `tx_id` and this network's genesis commitment.
    async fn seal(
        &self,
        tx_id: TransactionId,
        plaintext: &[u8],
    ) -> proto::transaction::SealedTransactionInputs {
        let attested = self
            .server
            .attested_encryption_key_schedule()
            .await
            .expect("the test schedule should attest");
        let key = attested.schedule.current_key.clone();
        let associated_data = transaction_inputs_associated_data(
            key.scheme.as_u32(),
            &key.key_id,
            self.server.genesis_commitment,
            tx_id,
        );
        let sealed = test_decrypter()
            .sealing_key()
            .seal_bytes_with_associated_data(&mut rand::rng(), plaintext, &associated_data)
            .expect("sealing should succeed");

        proto::transaction::SealedTransactionInputs {
            key_id: key.key_id.clone(),
            ciphertext: sealed.to_bytes(),
        }
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

    /// Returns whether `tx_id` has a validated transaction marker.
    async fn transaction_exists(&self, tx_id: TransactionId) -> bool {
        self.server
            .db
            .read("transaction_exists", move |tx| transaction_exists(tx, tx_id))
            .await
            .unwrap()
    }

    /// Returns the persisted validated transaction count.
    async fn validated_transaction_count(&self) -> i64 {
        self.server
            .db
            .read("count_validated_transactions", count_validated_transactions)
            .await
            .unwrap()
    }

    /// Asserts that a rejected transaction did not change either validated count.
    async fn assert_transaction_absent(&self, tx_id: TransactionId, expected_count: i64) {
        assert!(!self.transaction_exists(tx_id).await);
        assert_eq!(self.validated_transaction_count().await, expected_count);
        assert_eq!(
            self.call_status().await.validated_transactions_count,
            u64::try_from(expected_count).unwrap(),
        );
    }

    /// Calls the `get_transaction_encryption_key` endpoint on the validator server.
    async fn call_get_transaction_encryption_key(
        &self,
    ) -> proto::transaction::TransactionEncryptionKeyResponse {
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
    let genesis_state = GenesisState::new(
        vec![],
        test_fee_params(),
        1,
        0,
        ValidatorKeys::new(vec![key.public_key()]).unwrap(),
    );
    let genesis_block = genesis_state.into_block().unwrap();
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

/// Builds a syntactically valid [`ProvenTransaction`] with a dummy proof.
fn dummy_proven_tx(seed: u8) -> ProvenTransaction {
    let account_update = TxAccountUpdate::new(
        miden_protocol::testing::account_id::ACCOUNT_ID_PRIVATE_SENDER
            .try_into()
            .unwrap(),
        Word::empty(),
        Word::from([u32::from(seed), 0, 0, 0]),
        Word::empty(),
        AccountUpdateDetails::Private,
    )
    .unwrap();

    // The account state changes, which is what keeps this from being rejected as an empty
    // transaction; no input or output notes are needed.
    ProvenTransaction::new(
        account_update,
        Vec::<InputNoteCommitment>::new(),
        Vec::<OutputNote>::new(),
        BlockNumber::GENESIS,
        Word::empty(),
        BlockNumber::from(u32::from(seed) + 1),
        ExecutionProof::new_dummy(),
    )
    .unwrap()
}

/// A proven transaction and alternate inputs used to reach each validation stage.
struct ProvenTransactionFixture {
    transaction: ProvenTransaction,
    inputs: TransactionInputs,
    execution_failure_inputs: TransactionInputs,
    mismatch_inputs: TransactionInputs,
}

/// Builds one real proof and two alternate, well-formed input sets.
async fn proven_transaction_fixture() -> &'static ProvenTransactionFixture {
    static FIXTURE: OnceCell<ProvenTransactionFixture> = OnceCell::const_new();

    FIXTURE
        .get_or_init(|| async {
            let mut chain_builder = MockChainBuilder::new();
            let auth = Auth::BasicAuth {
                auth_scheme: AuthScheme::Falcon512Poseidon2,
            };
            let account_a = chain_builder.add_existing_wallet(auth.clone()).unwrap();
            let account_b = chain_builder.add_existing_wallet(auth).unwrap();
            assert_ne!(account_a.id(), account_b.id());

            let asset: Asset =
                FungibleAsset::new(ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET.try_into().unwrap(), 100)
                    .unwrap()
                    .into();
            let note_a = chain_builder
                .add_p2id_note(
                    ACCOUNT_ID_SENDER.try_into().unwrap(),
                    account_a.id(),
                    &[asset],
                    NoteType::Private,
                )
                .unwrap();
            let note_b = chain_builder
                .add_p2id_note(
                    ACCOUNT_ID_SENDER.try_into().unwrap(),
                    account_b.id(),
                    &[asset],
                    NoteType::Private,
                )
                .unwrap();
            let chain = chain_builder.build().unwrap();

            let context_a = chain
                .build_tx_context(account_a.id(), &[note_a.id()], &[])
                .unwrap()
                .build()
                .unwrap();
            let executed_a = Box::pin(context_a.execute()).await.unwrap();
            let inputs = executed_a.tx_inputs().clone();
            let transaction = LocalTransactionProver::default().prove(inputs.clone()).unwrap();

            let context_b = chain
                .build_tx_context(account_b.id(), &[note_b.id()], &[])
                .unwrap()
                .build()
                .unwrap();
            let mismatch_inputs = Box::pin(context_b.execute()).await.unwrap().tx_inputs().clone();
            let mut execution_failure_inputs = inputs.clone();
            execution_failure_inputs.set_input_notes(vec![note_b]);

            ProvenTransactionFixture {
                transaction,
                inputs,
                execution_failure_inputs,
                mismatch_inputs,
            }
        })
        .await
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
    assert!(
        !genesis_header.validator_keys().as_keys().contains(&rogue_signer.public_key()),
        "test requires a signing key that is not a member of the genesis validator set",
    );

    let result = ValidatorService::new(
        rogue_signer,
        std::sync::Arc::new(test_decrypter()),
        PrivateRecordSealer::from_operator_key(&operator_keys().remove(0)),
        db,
        block_store,
        InitialMetrics::new(0, 0, 0),
    )
    .await;
    assert!(
        matches!(result, Err(ValidatorError::ValidatorKeyNotInSet { .. })),
        "expected ValidatorKeyNotInSet error",
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
    let other_genesis_state = GenesisState::new(
        vec![],
        test_fee_params(),
        1,
        1,
        ValidatorKeys::new(vec![other_genesis_signer.public_key()]).unwrap(),
    );
    let other_genesis_block = other_genesis_state.into_block().unwrap();
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
    let other_genesis_state = GenesisState::new(
        vec![],
        test_fee_params(),
        1,
        1,
        ValidatorKeys::new(vec![other_genesis_signer.public_key()]).unwrap(),
    );
    let other_genesis_block = other_genesis_state.into_block().unwrap();
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

/// The endpoint returns one complete provider schedule attested by this validator. The shared
/// verifier consumes only the response and chain state already trusted by the caller.
#[tokio::test]
async fn transaction_encryption_key_schedule_is_attested() {
    let tv = TestValidator::new().await;
    let response = tv.call_get_transaction_encryption_key().await;
    let expected = test_decrypter()
        .encryption_key_schedule(tv.chain_tip.block_num())
        .await
        .unwrap();
    let current_key = response.current_key.as_ref().expect("response must carry a current key");
    let scheme = TransactionEncryptionScheme::try_from(current_key.scheme).unwrap();
    assert_eq!(scheme, expected.current_key.scheme);
    assert_eq!(current_key.key_id, expected.current_key.key_id);
    assert_eq!(current_key.public_key, expected.current_key.public_key);

    let [attestation] = response.attestations.as_slice() else {
        panic!("response must carry exactly the serving validator's attestation");
    };
    assert_eq!(
        attestation.validator_public_key,
        tv.server.signer.public_key().to_bytes(),
        "attestation must identify the serving validator",
    );

    let trusted_keys = [tv.server.signer.public_key()];
    let verified = verify_transaction_encryption_key_schedule(
        &response,
        TrustedTransactionEncryptionState::new(
            tv.chain_tip.commitment(),
            tv.chain_tip.block_num(),
            &trusted_keys,
        ),
    )
    .expect("attestation must verify against this validator's signing key");
    assert_eq!(verified.schedule(), &expected);
}

/// Validators sharing an encryption provider return the same schedule but attest it with their own
/// chain-recognized signing keys.
#[tokio::test]
async fn shared_schedule_is_attested_per_validator() {
    let tv_a = TestValidator::new().await;
    let tv_b = TestValidator::new().await;

    let response_a = tv_a.call_get_transaction_encryption_key().await;
    let response_b = tv_b.call_get_transaction_encryption_key().await;

    assert_eq!(response_a.current_key, response_b.current_key);
    assert_eq!(
        response_a.current_key_activation_block_num,
        response_b.current_key_activation_block_num
    );
    assert_eq!(response_a.next_key, response_b.next_key);
    assert_ne!(
        response_a.attestations[0].signature, response_b.attestations[0].signature,
        "each validator must attest with its own signing key",
    );
}

/// Changing a signed field invalidates the single schedule-level attestation.
#[tokio::test]
async fn tampered_schedule_fails_shared_verification() {
    let tv = TestValidator::new().await;
    let genesis = tv.chain_tip.commitment();
    let response = tv.call_get_transaction_encryption_key().await;
    let trusted_keys = [tv.server.signer.public_key()];
    let trusted =
        TrustedTransactionEncryptionState::new(genesis, tv.chain_tip.block_num(), &trusted_keys);

    let mut changed_key_id = response.clone();
    changed_key_id.current_key.as_mut().unwrap().key_id[0] ^= 0x01;
    let mut changed_public_key = response.clone();
    changed_public_key.current_key.as_mut().unwrap().public_key =
        KeyExchangeKey::read_from_bytes(&[4u8; 32]).unwrap().public_key().to_bytes();
    // Injecting a scheduled rotation into a schedule attested without one must also break the
    // signature, which is what makes the next key impossible to add or strip in transit.
    let mut injected_next_key = response.clone();
    injected_next_key.next_key = Some(proto::transaction::NextTransactionEncryptionKey {
        key: Some(proto::transaction::TransactionEncryptionKey {
            scheme: TransactionEncryptionScheme::X25519XChaCha20Poly1305.as_i32(),
            key_id: vec![9, 9, 9, 9],
            public_key: KeyExchangeKey::read_from_bytes(&[5u8; 32])
                .unwrap()
                .public_key()
                .to_bytes(),
        }),
        activation_block_num: BlockNumber::from_epoch(1).as_u32(),
    });

    for tampered in [changed_key_id, changed_public_key, injected_next_key] {
        assert!(
            verify_transaction_encryption_key_schedule(&tampered, trusted).is_err(),
            "attestation must not verify over tampered fields",
        );
    }

    let tampered_genesis = Word::try_from([9u64, 9, 9, 9]).unwrap();
    assert!(
        verify_transaction_encryption_key_schedule(
            &response,
            TrustedTransactionEncryptionState::new(
                tampered_genesis,
                tv.chain_tip.block_num(),
                &trusted_keys,
            ),
        )
        .is_err(),
        "attestation must not verify for another network",
    );
}

/// A fixed provider key remains current across epochs while the validator refreshes only the
/// schedule's freshness attestation.
#[tokio::test]
async fn schedule_is_reattested_without_automatic_rotation() {
    let tv = TestValidator::new().await;
    let before = tv.call_get_transaction_encryption_key().await;

    tv.server.committed_tip.send_replace(BlockNumber::from_epoch(1));
    let after = tv.call_get_transaction_encryption_key().await;

    assert_eq!(before.current_key, after.current_key);
    assert_eq!(before.next_key, after.next_key);
    assert_eq!(before.attestation_epoch, 0);
    assert_eq!(after.attestation_epoch, 1);
    assert_ne!(before.attestations[0].signature, after.attestations[0].signature);

    let validator_keys = [tv.server.signer.public_key()];
    let trusted = TrustedTransactionEncryptionState::new(
        tv.chain_tip.commitment(),
        BlockNumber::from_epoch(1),
        &validator_keys,
    );
    verify_transaction_encryption_key_schedule(&after, trusted).unwrap();
    assert!(verify_transaction_encryption_key_schedule(&before, trusted).is_err());
}

/// A request that waits on the cache lock reads the chain tip after the lock is acquired, so it
/// cannot replace a newer attestation with one for an older epoch.
#[tokio::test]
async fn stale_request_cannot_roll_back_schedule_attestation() {
    let tv = TestValidator::new().await;
    let epoch_one = ValidatorService::attest_encryption_key_schedule(
        tv.server.signer.as_ref(),
        tv.server.decrypter.as_ref(),
        tv.server.genesis_commitment,
        BlockNumber::from_epoch(1),
        tv.server.encryption_key_refresh_timeout,
    )
    .await
    .unwrap();

    let mut cached = tv.server.encryption_key_schedule.lock().await;
    let mut stale_request = Box::pin(tv.server.attested_encryption_key_schedule());
    tokio::select! {
        biased;
        _ = &mut stale_request => panic!("request unexpectedly completed"),
        () = tokio::task::yield_now() => {},
    }

    tv.server.committed_tip.send_replace(BlockNumber::from_epoch(1));
    cached.attested = Arc::new(epoch_one);
    drop(cached);

    let attested = stale_request.await.unwrap();
    assert_eq!(attested.epoch, 1);
    assert_eq!(tv.server.encryption_key_schedule.lock().await.attested.epoch, 1);
}

/// A failed epoch refresh is retried only after a request-path backoff, avoiding repeated provider
/// or KMS calls during an outage without introducing a background rotation worker.
#[tokio::test]
async fn failed_schedule_refresh_is_backed_off() {
    let provider = Arc::new(FailingScheduleProvider::new());
    let tv = TestValidator::new_with_decrypter(provider.clone()).await;
    assert_eq!(provider.schedule_calls.load(Ordering::SeqCst), 1);

    provider.fail_schedule.store(true, Ordering::SeqCst);
    tv.server.committed_tip.send_replace(BlockNumber::from_epoch(1));

    assert!(matches!(
        tv.server.attested_encryption_key_schedule().await,
        Err(ValidatorError::EncryptionKeyAttestationFailed(_))
    ));
    assert!(matches!(
        tv.server.attested_encryption_key_schedule().await,
        Err(ValidatorError::EncryptionKeyScheduleRefreshBackoff { epoch: 1 })
    ));
    assert_eq!(
        provider.schedule_calls.load(Ordering::SeqCst),
        2,
        "the initial load and first failed refresh should be the only provider calls",
    );
}

#[tokio::test]
async fn panicked_schedule_refresh_is_backed_off_without_wedging_cache() {
    let provider = Arc::new(FailingScheduleProvider::new());
    let tv = TestValidator::new_with_decrypter(provider.clone()).await;

    provider.panic_schedule.store(true, Ordering::SeqCst);
    tv.server.committed_tip.send_replace(BlockNumber::from_epoch(1));
    assert!(matches!(
        tv.server.attested_encryption_key_schedule().await,
        Err(ValidatorError::EncryptionKeyAttestationFailed(message))
            if message.contains("refresh task failed")
    ));
    assert!(matches!(
        tv.server.attested_encryption_key_schedule().await,
        Err(ValidatorError::EncryptionKeyScheduleRefreshBackoff { epoch: 1 })
    ));

    provider.panic_schedule.store(false, Ordering::SeqCst);
    tv.server.committed_tip.send_replace(BlockNumber::from_epoch(2));
    tv.server.attested_encryption_key_schedule().await.unwrap();
    assert_eq!(provider.schedule_calls.load(Ordering::SeqCst), 3);
}

/// A client cancellation does not penalize the next request.
#[tokio::test]
async fn cancelled_schedule_refresh_allows_immediate_retry() {
    let provider = Arc::new(FailingScheduleProvider::new());
    let tv = TestValidator::new_with_decrypter(provider.clone()).await;

    provider.block_schedule.store(true, Ordering::SeqCst);
    tv.server.committed_tip.send_replace(BlockNumber::from_epoch(1));

    let mut refresh = Box::pin(tv.server.attested_encryption_key_schedule());
    tokio::select! {
        () = provider.schedule_started.notified() => {},
        _ = &mut refresh => panic!("refresh unexpectedly completed"),
    }
    drop(refresh);

    provider.block_schedule.store(false, Ordering::SeqCst);
    provider.schedule_released.notify_one();
    tv.server.attested_encryption_key_schedule().await.unwrap();
    assert_eq!(
        provider.schedule_calls.load(Ordering::SeqCst),
        2,
        "the successful retry must share the refresh started by the cancelled request",
    );
}

#[tokio::test]
async fn failed_signer_refresh_is_backed_off() {
    let mut tv = TestValidator::new().await;
    let public_key = tv.server.signer.public_key();
    tv.server.signer = Arc::new(ValidatorSigner::new_failing(public_key));
    tv.server.committed_tip.send_replace(BlockNumber::from_epoch(1));

    assert!(matches!(
        tv.server.attested_encryption_key_schedule().await,
        Err(ValidatorError::EncryptionKeyAttestationFailed(_))
    ));
    assert!(matches!(
        tv.server.attested_encryption_key_schedule().await,
        Err(ValidatorError::EncryptionKeyScheduleRefreshBackoff { epoch: 1 })
    ));
}

#[tokio::test]
async fn schedule_lookup_timeout_is_backed_off() {
    let provider = Arc::new(FailingScheduleProvider::new());
    let mut tv = TestValidator::new_with_decrypter(provider.clone()).await;
    tv.server.encryption_key_refresh_timeout = Duration::from_millis(1);
    provider.block_schedule.store(true, Ordering::SeqCst);
    tv.server.committed_tip.send_replace(BlockNumber::from_epoch(1));

    assert!(matches!(
        tv.server.attested_encryption_key_schedule().await,
        Err(ValidatorError::EncryptionKeyScheduleRefreshTimedOut { operation: "loading" })
    ));
    assert!(matches!(
        tv.server.attested_encryption_key_schedule().await,
        Err(ValidatorError::EncryptionKeyScheduleRefreshBackoff { epoch: 1 })
    ));
    assert_eq!(provider.schedule_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn schedule_signing_timeout_is_backed_off() {
    let mut tv = TestValidator::new().await;
    tv.server.encryption_key_refresh_timeout = Duration::from_millis(1);
    let public_key = tv.server.signer.public_key();
    tv.server.signer = Arc::new(ValidatorSigner::new_blocking(public_key));
    tv.server.committed_tip.send_replace(BlockNumber::from_epoch(1));

    assert!(matches!(
        tv.server.attested_encryption_key_schedule().await,
        Err(ValidatorError::EncryptionKeyScheduleRefreshTimedOut { operation: "signing" })
    ));
    assert!(matches!(
        tv.server.attested_encryption_key_schedule().await,
        Err(ValidatorError::EncryptionKeyScheduleRefreshBackoff { epoch: 1 })
    ));
}

/// A client can reconstruct the sealing key from the response and the provider can decrypt the
/// ciphertext selected by the caller-supplied opaque key id.
#[tokio::test]
async fn response_key_seals_for_the_validator_set() {
    use miden_protocol::crypto::dsa::eddsa_25519_sha512::PublicKey as EncryptionPublicKey;
    use miden_protocol::crypto::ies::SealingKey;

    let tv = TestValidator::new().await;
    let response = tv.call_get_transaction_encryption_key().await;
    let current = response.current_key.unwrap();

    let public_key = EncryptionPublicKey::read_from_bytes(&current.public_key)
        .expect("response public key should deserialize");
    let sealing_key = SealingKey::X25519XChaCha20Poly1305(public_key);
    let associated_data = b"scheme|key_id|chain|tx";
    let sealed = sealing_key
        .seal_bytes_with_associated_data(&mut rand::rng(), b"transaction inputs", associated_data)
        .unwrap()
        .to_bytes();

    let opened = test_decrypter()
        .decrypt_transaction_inputs(
            &current.key_id,
            tv.chain_tip.block_num(),
            &sealed,
            associated_data,
        )
        .await
        .unwrap();
    assert_eq!(opened, b"transaction inputs");
}

/// Like status, the encryption key remains available during an exclusive backup subscription.
#[tokio::test]
async fn encryption_key_available_during_backup() {
    let mut tv = TestValidator::new().await;
    tv.apply_empty_block().await;
    let stream = tv.call_block_subscription(1).await;

    let response = tv.call_get_transaction_encryption_key().await;
    assert!(!response.current_key.unwrap().public_key.is_empty());

    drop(stream);
}

// SUBMIT PATH: TRANSACTION INPUT SEALING
// ================================================================================================

/// A submission with no encrypted inputs is rejected before validation.
#[tokio::test]
async fn submit_rejects_missing_encrypted_inputs() {
    let tv = TestValidator::new().await;
    let tx = dummy_proven_tx(2);
    let request = tonic::Request::new(proto::transaction::ProvenTransaction {
        transaction: tx.to_bytes(),
        sealed_transaction_inputs: None,
    });

    let status = validator_api::SubmitProvenTransaction::full(&tv.server, request)
        .await
        .unwrap_err();

    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert!(status.message().contains("Missing sealed transaction inputs"));
    tv.assert_transaction_absent(tx.id(), 0).await;
}

/// Plaintext transaction inputs must be impossible to submit. This is the central guarantee of the
/// whole change.
#[tokio::test]
async fn submit_rejects_plaintext_inputs() {
    let tv = TestValidator::new().await;
    let tx = dummy_proven_tx(3);
    let sealed = proto::transaction::SealedTransactionInputs {
        key_id: tv.current_key_id().await,
        ciphertext: b"not a sealed message, just bytes".to_vec(),
    };

    let status = tv.call_submit_proven_transaction(&tx, sealed).await.unwrap_err();

    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert!(status.message().contains("unseal"), "got: {}", status.message());
    tv.assert_transaction_absent(tx.id(), 0).await;
}

/// A key id that does not match the validator's earns a distinct, actionable status so a client
/// knows to re-fetch rather than retry the same blob, without disclosing the validator's own key
/// id.
#[tokio::test]
async fn submit_rejects_unknown_key_id() {
    let tv = TestValidator::new().await;
    let tx = dummy_proven_tx(4);
    let mut sealed = tv.seal(tx.id(), b"transaction inputs").await;
    sealed.key_id = vec![0xAA, 0xBB, 0xCC, 0xDD];

    let status = tv.call_submit_proven_transaction(&tx, sealed).await.unwrap_err();

    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
    assert!(
        status.message().contains("GetTransactionEncryptionKey"),
        "the rejection must tell the client to re-fetch the key, got: {}",
        status.message(),
    );
    // This status reaches the client verbatim through the RPC.
    let own_key_id = hex::encode(tv.current_key_id().await);
    assert!(
        !status.message().contains(&own_key_id),
        "the rejection must not echo the validator's key id",
    );
    tv.assert_transaction_absent(tx.id(), 0).await;
}

/// The validator enforces the associated data, so a ciphertext captured from one transaction cannot
/// be replayed onto another. Which fields the transcript covers is pinned separately by the golden
/// vector in `miden_node_proto::domain::encryption`.
#[tokio::test]
async fn submit_rejects_inputs_sealed_for_a_different_transaction() {
    let tv = TestValidator::new().await;
    let tx_a = dummy_proven_tx(6);
    let tx_b = dummy_proven_tx(7);
    assert_ne!(tx_a.id(), tx_b.id());

    let sealed_for_a = tv.seal(tx_a.id(), b"transaction inputs").await;

    let status = tv.call_submit_proven_transaction(&tx_b, sealed_for_a).await.unwrap_err();

    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert!(status.message().contains("unseal"), "got: {}", status.message());
    tv.assert_transaction_absent(tx_b.id(), 0).await;
}

/// Correctly sealed inputs get past the unseal and fail later, at deserialization. Without this the
/// tests above would all still pass if the unseal simply always failed.
#[tokio::test]
async fn correctly_sealed_inputs_reach_the_deserialization_stage() {
    let tv = TestValidator::new().await;
    let tx = dummy_proven_tx(10);
    let sealed = tv.seal(tx.id(), b"not really transaction inputs").await;

    let status = tv.call_submit_proven_transaction(&tx, sealed).await.unwrap_err();

    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert!(
        status.message().contains("Invalid transaction inputs"),
        "the unseal should have succeeded and failed at deserialization instead, got: {}",
        status.message(),
    );
    assert!(
        !status.message().contains("unseal"),
        "the unseal must have succeeded, got: {}",
        status.message(),
    );
    tv.assert_transaction_absent(tx.id(), 0).await;
}

/// A failed proof must not store the authenticated transaction inputs.
#[tokio::test]
async fn failed_proof_verification_does_not_store_inputs() {
    let tv = TestValidator::new().await;
    let tx = dummy_proven_tx(11);
    let fixture = proven_transaction_fixture().await;
    let sealed = tv.seal(tx.id(), &fixture.inputs.to_bytes()).await;

    let status = tv.call_submit_proven_transaction(&tx, sealed).await.unwrap_err();

    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert!(status.message().contains("proof verification"), "got: {}", status.message());
    tv.assert_transaction_absent(tx.id(), 0).await;
}

/// A transaction that cannot be re-executed must not create a sealed record.
#[tokio::test]
async fn failed_reexecution_does_not_store_inputs() {
    let tv = TestValidator::new().await;
    let fixture = proven_transaction_fixture().await;
    let tx = &fixture.transaction;
    let sealed = tv.seal(tx.id(), &fixture.execution_failure_inputs.to_bytes()).await;

    let status = tv.call_submit_proven_transaction(tx, sealed).await.unwrap_err();

    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert!(status.message().contains("re-executed"), "got: {}", status.message());
    tv.assert_transaction_absent(tx.id(), 0).await;
}

/// A successful re-execution with a different header must not create a sealed record.
#[tokio::test]
async fn header_mismatch_does_not_store_inputs() {
    let tv = TestValidator::new().await;
    let fixture = proven_transaction_fixture().await;
    let tx = &fixture.transaction;
    let sealed = tv.seal(tx.id(), &fixture.mismatch_inputs.to_bytes()).await;

    let status = tv.call_submit_proven_transaction(tx, sealed).await.unwrap_err();

    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert!(status.message().contains("did not match"), "got: {}", status.message());
    tv.assert_transaction_absent(tx.id(), 0).await;
}

/// A valid submission stores one marker and one protected record.
#[tokio::test]
async fn valid_submission_stores_one_protected_record() {
    let tv = TestValidator::new().await;
    let fixture = proven_transaction_fixture().await;
    let tx = &fixture.transaction;
    let first = tv.seal(tx.id(), &fixture.inputs.to_bytes()).await;
    let second = tv.seal(tx.id(), &fixture.inputs.to_bytes()).await;
    assert_ne!(first.ciphertext, second.ciphertext);

    tv.call_submit_proven_transaction(tx, first.clone()).await.unwrap();
    let transaction_id = tx.id();
    let first_record = tv
        .server
        .db
        .read("load_private_record", move |db_tx| load_private_record(db_tx, transaction_id))
        .await
        .unwrap()
        .unwrap();
    tv.call_submit_proven_transaction(tx, second).await.unwrap();

    assert!(tv.transaction_exists(tx.id()).await);
    let stored_record = tv
        .server
        .db
        .read("load_private_record", move |db_tx| load_private_record(db_tx, transaction_id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored_record, first_record);
    assert_eq!(tv.validated_transaction_count().await, 1);
    assert_eq!(tv.call_status().await.validated_transactions_count, 1);
}

/// `ValidatorClient::submit_batch` forwards items through this handler one at a time. A failed item
/// must not create its own record when another item in that sequence succeeds.
#[tokio::test]
async fn failed_batch_item_does_not_store_inputs() {
    let tv = TestValidator::new().await;
    let fixture = proven_transaction_fixture().await;
    let valid_tx = &fixture.transaction;
    let rejected_tx = dummy_proven_tx(12);

    tv.call_submit_proven_transaction(
        valid_tx,
        tv.seal(valid_tx.id(), &fixture.inputs.to_bytes()).await,
    )
    .await
    .unwrap();
    let status = tv
        .call_submit_proven_transaction(
            &rejected_tx,
            tv.seal(rejected_tx.id(), &fixture.inputs.to_bytes()).await,
        )
        .await
        .unwrap_err();

    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    tv.assert_transaction_absent(rejected_tx.id(), 1).await;
    assert!(tv.transaction_exists(valid_tx.id()).await);
}
