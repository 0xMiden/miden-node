//! Drives increments against the seeded counter account.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use miden_protocol::account::auth::AuthSecretKey;
use miden_protocol::account::{
    Account,
    AccountId,
    PartialAccount,
    StorageMapKey,
    StorageMapWitness,
};
use miden_protocol::asset::{AssetId, AssetWitness};
use miden_protocol::block::{BlockHeader, BlockNumber};
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
use miden_protocol::note::{
    Note,
    NoteAssets,
    NoteAttachment,
    NoteAttachments,
    NoteRecipient,
    NoteScript,
    NoteScriptRoot,
    NoteStorage,
    NoteType,
    PartialNote,
    PartialNoteMetadata,
};
use miden_protocol::transaction::{
    AccountInputs,
    InputNotes,
    PartialBlockchain,
    TransactionArgs,
    TransactionScript,
};
use miden_protocol::utils::serde::Serializable;
use miden_protocol::{Felt, Word};
use miden_standards::code_builder::CodeBuilder;
use miden_standards::note::{NetworkAccountTarget, NoteExecutionHint};
use miden_tx::auth::BasicAuthenticator;
use miden_tx::{
    DataStore,
    DataStoreError,
    LoadedMastForest,
    LocalTransactionProver,
    MastForestStore,
    TransactionExecutor,
    TransactionMastStore,
};
use rand::RngExt;
use rand_chacha::ChaCha20Rng;

use crate::accounts::{
    COUNTER_SLOT,
    WALLET_COUNTER_COMPONENT_PATH,
    create_increment_script,
    wallet_counter_component_code,
};
use crate::rpc::SubmissionClient;

/// Everything one increment needs, carried across iterations of the loop.
pub struct Driver {
    wallet: Account,
    counter: Account,
    secret_key: SecretKey,
    increment_script: NoteScript,
    genesis_header: BlockHeader,
    prover: LocalTransactionProver,
    rng: ChaCha20Rng,
}

impl Driver {
    pub fn new(
        wallet: Account,
        counter: Account,
        secret_key: SecretKey,
        client: &SubmissionClient,
        rng: ChaCha20Rng,
    ) -> Result<Self> {
        Ok(Self {
            wallet,
            counter,
            secret_key,
            increment_script: create_increment_script()
                .context("failed to compile the increment note script")?,
            genesis_header: client.genesis_header().clone(),
            prover: LocalTransactionProver::default(),
            rng,
        })
    }

    /// The wallet in its current local state, for persisting between increments.
    pub fn wallet(&self) -> &Account {
        &self.wallet
    }

    /// Reads the counter account's on-chain value.
    ///
    /// This only advances once the ntx-builder has loaded the large account and consumed one of the
    /// network notes, so it is the signal that the harness is exercising what it means to.
    pub async fn observed_counter(&self, client: &SubmissionClient) -> Result<Option<u64>> {
        client.slot_value(self.counter.id(), COUNTER_SLOT).await
    }

    /// Builds, proves, and submits one increment, then advances the local wallet by the resulting
    /// patch. Returns the accepted block height and how long proving took.
    pub async fn submit_one(&mut self, client: &SubmissionClient) -> Result<Submitted> {
        let (network_note, recipient) = create_network_note(
            &self.wallet,
            &self.counter,
            self.increment_script.clone(),
            &mut self.rng,
        )?;

        let script = create_increment_tx_script(&network_note)?;
        let mut tx_args = TransactionArgs::default().with_tx_script(script);
        tx_args.add_output_note_recipient(Box::new(recipient));

        let mut data_store =
            DriverDataStore::new(self.genesis_header.clone(), PartialBlockchain::default());
        data_store.add_account(self.wallet.clone());
        data_store.add_account(self.counter.clone());

        let authenticator =
            BasicAuthenticator::new(&[AuthSecretKey::Falcon512Poseidon2(self.secret_key.clone())]);
        let executor = TransactionExecutor::new(&data_store).with_authenticator(&authenticator);

        let executed = executor
            .execute_transaction(
                self.wallet.id(),
                self.genesis_header.block_num(),
                InputNotes::default(),
                tx_args,
            )
            .await
            .context("failed to execute the increment transaction")?;

        let tx_inputs = executed.tx_inputs().to_bytes();
        let patch = executed.account_patch().clone();

        let proving_started = Instant::now();
        let proven = self.prover.prove(executed).context("failed to prove the transaction")?;
        let proving_time = proving_started.elapsed();

        let block_num = client.submit(&proven, &tx_inputs).await?;

        self.wallet
            .apply_patch(&patch)
            .context("failed to apply the transaction patch to the local wallet")?;

        Ok(Submitted {
            tx_id: proven.id().to_hex(),
            block_num,
            proving_time,
        })
    }
}

/// The outcome of one accepted increment.
pub struct Submitted {
    pub tx_id: String,
    pub block_num: BlockNumber,
    pub proving_time: Duration,
}

// NOTE + SCRIPT CONSTRUCTION
// ================================================================================================

/// Builds the network note addressed to the counter account.
///
/// The `NetworkAccountTarget` attachment is what makes this a *network* note: the ntx-builder watches
/// for notes targeting network accounts and authors the consuming transaction itself.
fn create_network_note(
    wallet: &Account,
    counter: &Account,
    script: NoteScript,
    rng: &mut ChaCha20Rng,
) -> Result<(Note, NoteRecipient)> {
    let target = NetworkAccountTarget::new(counter.id(), NoteExecutionHint::Always)
        .context("counter account should be a valid network target")?;
    let attachment: NoteAttachment = target.into();
    let attachments = NoteAttachments::from(attachment);

    let partial_metadata = PartialNoteMetadata::new(wallet.id(), NoteType::Public);

    let serial_num = Word::new([
        Felt::new_unchecked(rng.random()),
        Felt::new_unchecked(rng.random()),
        Felt::new_unchecked(rng.random()),
        Felt::new_unchecked(rng.random()),
    ]);

    let recipient = NoteRecipient::new(serial_num, script, NoteStorage::new(vec![])?);
    let note = Note::with_attachments(
        NoteAssets::new(vec![])?,
        partial_metadata,
        recipient.clone(),
        attachments,
    );

    Ok((note, recipient))
}

/// Builds the transaction script for one increment.
///
/// The whole transaction is a single `call` into the wallet's `increment_and_create_note` procedure,
/// which creates the network note and bumps the wallet's counter slot atomically.
fn create_increment_tx_script(network_note: &Note) -> Result<TransactionScript> {
    let wallet_component = wallet_counter_component_code()?;

    let partial: PartialNote = network_note.clone().into();
    let recipient = partial.recipient_digest();
    let note_type = Felt::from(partial.metadata().note_type());
    let tag = Felt::from(partial.metadata().tag());

    // `increment_and_create_note` shares `create_note`'s stack contract: it consumes `[tag,
    // note_type, RECIPIENT, pad(10)]` and returns `[note_idx, pad(15)]`. The padding is built
    // explicitly and the trailing pads reduced back to `[note_idx]`, otherwise they survive on the
    // overflow stack and `main` returns at the wrong depth.
    let call_target = format!("::{WALLET_COUNTER_COMPONENT_PATH}::increment_and_create_note");
    let mut note_section = format!(
        "
        padw padw push.0.0
        push.{recipient}
        push.{note_type}
        push.{tag}
        # => [tag, note_type, RECIPIENT, pad(10)]
        call.{call_target}
        # => [note_idx, pad(15)]
        movdn.15 dropw dropw dropw drop drop drop
        # => [note_idx]
        "
    );

    for attachment in partial.attachments().iter() {
        let scheme = attachment.attachment_scheme().as_u16();
        let commitment = attachment.content().to_commitment();
        // `add_attachment` consumes `[attachment_scheme, ATTACHMENT_COMMITMENT, note_idx]`, so dup
        // the note index for it to consume and keep our own copy for the next attachment / the
        // drop.
        write!(
            note_section,
            "
        dup
        push.{commitment}
        push.{scheme}
        # => [attachment_scheme, ATTACHMENT_COMMITMENT, note_idx, note_idx]
        exec.::miden::protocol::output_note::add_attachment
        # => [note_idx]
        "
        )
        .expect("writing to a String cannot fail");
    }
    note_section.push_str("        drop\n");

    let script_src = format!(
        "@transaction_script
        pub proc main
{note_section}
        end"
    );

    let mut code_builder = CodeBuilder::new()
        .with_dynamically_linked_library(&wallet_component)
        .context("failed to dynamically link the wallet counter component")?;

    // Attachments are resolved at runtime from the advice map, keyed by their commitment.
    for attachment in partial.attachments().iter() {
        code_builder.add_advice_map_entry(attachment.to_commitment(), attachment.to_elements());
    }

    code_builder
        .compile_tx_script(script_src)
        .context("failed to compile the increment transaction script")
}

// DATA STORE
// ================================================================================================

/// An in-memory [`DataStore`] over the genesis header and the two accounts involved.
///
/// The transaction consumes no input notes and touches no foreign accounts or storage maps, so only
/// the account, blockchain, and vault-witness methods need real implementations.
struct DriverDataStore {
    accounts: HashMap<AccountId, Account>,
    block_header: BlockHeader,
    partial_blockchain: PartialBlockchain,
    mast_store: TransactionMastStore,
}

impl DriverDataStore {
    fn new(block_header: BlockHeader, partial_blockchain: PartialBlockchain) -> Self {
        Self {
            accounts: HashMap::new(),
            block_header,
            partial_blockchain,
            mast_store: TransactionMastStore::new(),
        }
    }

    fn add_account(&mut self, account: Account) {
        self.mast_store.load_account_code(account.code());
        self.accounts.insert(account.id(), account);
    }

    fn account(&self, account_id: AccountId) -> Result<&Account, DataStoreError> {
        self.accounts.get(&account_id).ok_or_else(|| DataStoreError::Other {
            error_msg: "unknown account".into(),
            source: None,
        })
    }
}

impl DataStore for DriverDataStore {
    async fn get_transaction_inputs(
        &self,
        account_id: AccountId,
        _block_refs: BTreeSet<BlockNumber>,
    ) -> Result<(PartialAccount, BlockHeader, PartialBlockchain), DataStoreError> {
        let account = self.account(account_id)?;

        Ok((
            PartialAccount::from(account),
            self.block_header.clone(),
            self.partial_blockchain.clone(),
        ))
    }

    async fn get_storage_map_witness(
        &self,
        _account_id: AccountId,
        _map_root: Word,
        _map_key: StorageMapKey,
    ) -> Result<StorageMapWitness, DataStoreError> {
        Err(DataStoreError::other("increment transactions do not read storage maps"))
    }

    async fn get_foreign_account_inputs(
        &self,
        _foreign_account_id: AccountId,
        _ref_block: BlockNumber,
    ) -> Result<AccountInputs, DataStoreError> {
        Err(DataStoreError::other("increment transactions use no foreign accounts"))
    }

    async fn get_vault_asset_witnesses(
        &self,
        account_id: AccountId,
        vault_root: Word,
        vault_keys: BTreeSet<AssetId>,
    ) -> Result<Vec<AssetWitness>, DataStoreError> {
        let account = self.account(account_id)?;

        if account.vault().root() != vault_root {
            return Err(DataStoreError::other("vault root mismatch"));
        }

        Result::<Vec<_>, _>::from_iter(vault_keys.into_iter().map(|vault_key| {
            AssetWitness::new(account.vault().open(vault_key).into(), [vault_key]).map_err(|err| {
                DataStoreError::Other {
                    error_msg: "failed to open the vault asset tree".into(),
                    source: Some(Box::new(err)),
                }
            })
        }))
    }

    async fn get_note_script(
        &self,
        _script_root: NoteScriptRoot,
    ) -> Result<Option<NoteScript>, DataStoreError> {
        Ok(None)
    }
}

impl MastForestStore for DriverDataStore {
    fn get(&self, procedure_hash: &Word) -> Option<LoadedMastForest> {
        self.mast_store.get(procedure_hash)
    }
}
