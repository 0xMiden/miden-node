use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Instant,
};

mod metrics;
use anyhow::Context;
use clap::{Parser, Subcommand};
use metrics::Metrics;
use miden_air::{FieldElement, HashFunction};
use miden_block_prover::LocalBlockProver;
use miden_lib::{
    account::{auth::RpoFalcon512, faucets::BasicFungibleFaucet, wallets::BasicWallet},
    note::create_p2id_note,
    utils::Serializable,
};
use miden_node_block_producer::store::StoreClient;
use miden_node_proto::{domain::batch::BatchInputs, generated::store::api_client::ApiClient};
use miden_node_store::{GenesisState, Store};
use miden_node_utils::tracing::grpc::OtelInterceptor;
use miden_objects::{
    Felt,
    account::{
        Account, AccountBuilder, AccountId, AccountIdAnchor, AccountStorageMode, AccountType,
    },
    asset::{Asset, FungibleAsset, TokenSymbol},
    batch::{BatchAccountUpdate, BatchId, ProvenBatch},
    block::{BlockHeader, BlockInputs, BlockNumber, ProposedBlock, ProvenBlock},
    crypto::{
        dsa::rpo_falcon512::{PublicKey, SecretKey},
        rand::RpoRandomCoin,
    },
    note::{Note, NoteHeader, NoteId, NoteInclusionProof},
    transaction::{
        InputNote, InputNotes, OutputNote, ProvenTransaction, ProvenTransactionBuilder,
        TransactionHeader,
    },
    vm::ExecutionProof,
};
use rand::Rng;
use rayon::{
    iter::{IntoParallelIterator, ParallelIterator},
    prelude::ParallelSlice,
};
use tokio::{net::TcpListener, task};
use winterfell::Proof;

// CONSTANTS
// ================================================================================================

const BATCHES_PER_BLOCK: usize = 16;
const TRANSACTIONS_PER_BATCH: usize = 16;

// CLI
// ================================================================================================

#[derive(Parser)]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Create and store blocks into the store. Create a given number of accounts, where each
    /// account consumes a note created from a faucet.
    SeedStore {
        /// Directory in which to store the database and raw block data. If the directory contains
        /// a database dump file, it will be replaced.
        #[arg(short, long, value_name = "DATA_DIRECTORY")]
        data_directory: PathBuf,

        /// Number of accounts to create.
        #[arg(short, long, value_name = "NUM_ACCOUNTS")]
        num_accounts: usize,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::SeedStore { data_directory, num_accounts } => {
            seed_store(data_directory, num_accounts).await;
        },
    }
}

// SEED STORE
// ================================================================================================

/// Seeds the store with a given number of accounts.
async fn seed_store(data_directory: PathBuf, num_accounts: usize) {
    let start = Instant::now();

    // Recreate the data directory (it should be empty for store bootstrapping).
    //
    // Ignore the error since it will also error if it does not exist.
    let _ = std::fs::remove_dir_all(&data_directory);
    std::fs::create_dir_all(&data_directory).expect("created data directory");

    // generate the faucet account and the genesis state
    let faucet = create_faucet();
    let genesis_state = GenesisState::new(vec![faucet.clone()], 1, 1);
    Store::bootstrap(genesis_state.clone(), &data_directory).expect("store should bootstrap");

    // start the store
    let store_addr = {
        let grpc_store = TcpListener::bind("127.0.0.1:0").await.expect("Failed to bind store");
        let store_addr = grpc_store.local_addr().expect("Failed to get store address");
        let store = Store::init(grpc_store, data_directory.clone())
            .await
            .expect("Failed to init store");
        task::spawn(async move { store.serve().await.context("Serving store") });
        store_addr
    };

    // connect to the store
    let store_client = {
        let channel = tonic::transport::Endpoint::try_from(format!("http://{store_addr}",))
            .unwrap()
            .connect()
            .await
            .expect("Failed to connect to store");
        let store_api_client = ApiClient::with_interceptor(channel, OtelInterceptor);
        StoreClient::new(store_api_client)
    };

    // start generating blocks
    let data_directory =
        miden_node_store::DataDirectory::load(data_directory).expect("data directory should exist");
    let genesis_header = genesis_state.into_block().unwrap().into_inner();
    let metrics = generate_blocks(
        num_accounts,
        faucet,
        genesis_header,
        &store_client,
        data_directory.database_path(),
    )
    .await;

    println!("Total time: {:.3} seconds", start.elapsed().as_secs_f64());
    println!("{metrics}");
}

/// Generates batches of transactions to be inserted into the store.
///
/// The first transaction in each batch sends assets from the faucet to 255 accounts.
/// The rest of the transactions consume the notes created by the faucet in the previous block.
async fn generate_blocks(
    num_accounts: usize,
    mut faucet: Account,
    genesis_block: ProvenBlock,
    store_client: &StoreClient,
    database_filepath: PathBuf,
) -> Metrics {
    // Each block is composed of [`BATCHES_PER_BLOCK`] batches, and each batch is composed of
    // [`TRANSACTIONS_PER_BATCH`] txs. The first note of the block is always a send assets tx
    // from the faucet to (BATCHES_PER_BLOCK * TRANSACTIONS_PER_BATCH) - 1 accounts. The rest of
    // the notes are consume note txs from the (BATCHES_PER_BLOCK * TRANSACTIONS_PER_BATCH) - 1
    // accounts that were minted in the previous block.
    let mut metrics = Metrics::new(database_filepath);

    let mut consume_notes_txs = vec![];

    let consumes_per_block = TRANSACTIONS_PER_BATCH * BATCHES_PER_BLOCK - 1;
    // +1 to account for the first block with the send assets tx only
    let total_blocks = (num_accounts / consumes_per_block) + 1;

    // share random coin seed and key pair for all accounts to avoid key generation overhead
    let coin_seed: [u64; 4] = rand::rng().random();
    let rng = Arc::new(Mutex::new(RpoRandomCoin::new(coin_seed.map(Felt::new))));
    let key_pair = {
        let mut rng = rng.lock().unwrap();
        SecretKey::with_rng(&mut *rng)
    };

    let mut prev_block = genesis_block.clone();
    let mut current_anchor_header = genesis_block.header().clone();

    for i in 0..total_blocks {
        let mut block_txs = Vec::with_capacity(BATCHES_PER_BLOCK * TRANSACTIONS_PER_BATCH);

        // create accounts and notes that mint assets for these accounts
        let (accounts, notes) = create_accounts_and_notes(
            consumes_per_block,
            &current_anchor_header,
            &key_pair,
            &rng,
            faucet.id(),
            i,
        );

        // create the tx that creates the notes
        let emit_note_tx = create_emit_note_tx(prev_block.header(), &mut faucet, notes.clone());

        // collect all the txs
        block_txs.push(emit_note_tx);
        block_txs.extend(consume_notes_txs);

        // create the batches with [TRANSACTIONS_PER_BATCH] txs each
        let batches: Vec<ProvenBatch> = block_txs
            .par_chunks(TRANSACTIONS_PER_BATCH)
            .map(|txs| create_batch(txs, prev_block.header()))
            .collect();

        // create the block and send it to the store
        let block_inputs = get_block_inputs(store_client, &batches, &mut metrics).await;

        // update blocks
        prev_block = apply_block(batches, block_inputs, store_client, &mut metrics).await;
        if current_anchor_header.block_epoch() != prev_block.header().block_epoch() {
            current_anchor_header = prev_block.header().clone();
        }

        // create the consume notes txs to be used in the next block
        let batch_inputs =
            get_batch_inputs(store_client, prev_block.header(), &notes, &mut metrics).await;
        consume_notes_txs = create_consume_note_txs(
            prev_block.header(),
            accounts,
            notes,
            &batch_inputs.note_proofs,
        );

        // track store size every 50 blocks
        if i % 50 == 0 {
            metrics.record_store_size();
        }
    }
    metrics
}

/// Given a list of batches and block inputs, creates a `ProvenBlock` and sends it to the store.
/// Tracks the insertion time on the metrics.
///
/// Returns the the inserted block.
async fn apply_block(
    batches: Vec<ProvenBatch>,
    block_inputs: BlockInputs,
    store_client: &StoreClient,
    metrics: &mut Metrics,
) -> ProvenBlock {
    let proposed_block = ProposedBlock::new(block_inputs, batches).unwrap();
    let proven_block = LocalBlockProver::new(0)
        .prove_without_batch_verification(proposed_block)
        .unwrap();
    let block_size: usize = proven_block.to_bytes().len();

    let start = Instant::now();
    store_client.apply_block(&proven_block).await.unwrap();
    metrics.track_block_insertion(start.elapsed(), block_size);

    proven_block
}

// HELPER FUNCTIONS
// ================================================================================================

/// Creates `num_accounts` accounts, and for each one creates a note that mint assets.
///
/// Returns a tuple with:
/// - The list of new accounts
/// - The list of new notes
fn create_accounts_and_notes(
    num_accounts: usize,
    anchor_block: &BlockHeader,
    key_pair: &SecretKey,
    rng: &Arc<Mutex<RpoRandomCoin>>,
    faucet_id: AccountId,
    block_num: usize,
) -> (Vec<Account>, Vec<Note>) {
    (0..num_accounts)
        .into_par_iter()
        .map(|account_index| {
            let account = create_account(
                anchor_block,
                key_pair.public_key(),
                ((block_num * num_accounts) + account_index) as u64,
            );
            let note = {
                let mut rng = rng.lock().unwrap();
                create_note(faucet_id, account.id(), &mut rng)
            };
            (account, note)
        })
        .collect()
}

/// Creates a public P2ID note containing 10 tokens of the fungible asset associated with the
/// specified `faucet_id` and sent to the specified target account.
fn create_note(faucet_id: AccountId, target_id: AccountId, rng: &mut RpoRandomCoin) -> Note {
    let asset = Asset::Fungible(FungibleAsset::new(faucet_id, 10).unwrap());
    create_p2id_note(
        faucet_id,
        target_id,
        vec![asset],
        miden_objects::note::NoteType::Public,
        Felt::default(),
        rng,
    )
    .expect("note creation failed")
}

/// Creates a new private account with a given public key and anchor block. Generates the seed from
/// the given index.
fn create_account(anchor_block: &BlockHeader, public_key: PublicKey, index: u64) -> Account {
    let init_seed: Vec<_> = index.to_be_bytes().into_iter().chain([0u8; 24]).collect();
    let (new_account, _) = AccountBuilder::new(init_seed.try_into().unwrap())
        .anchor(anchor_block.try_into().unwrap())
        .account_type(AccountType::RegularAccountImmutableCode)
        .storage_mode(AccountStorageMode::Private)
        .with_component(RpoFalcon512::new(public_key))
        .with_component(BasicWallet)
        .build()
        .unwrap();
    new_account
}

/// Creates a new faucet account.
fn create_faucet() -> Account {
    let coin_seed: [u64; 4] = rand::rng().random();
    let mut rng = RpoRandomCoin::new(coin_seed.map(Felt::new));
    let key_pair = SecretKey::with_rng(&mut rng);
    let init_seed = [0_u8; 32];

    let token_symbol = TokenSymbol::new("TEST").unwrap();
    let (new_faucet, _seed) = AccountBuilder::new(init_seed)
        .anchor(AccountIdAnchor::PRE_GENESIS)
        .account_type(AccountType::FungibleFaucet)
        .storage_mode(AccountStorageMode::Private)
        .with_component(RpoFalcon512::new(key_pair.public_key()))
        .with_component(BasicFungibleFaucet::new(token_symbol, 2, Felt::new(u64::MAX)).unwrap())
        .build()
        .unwrap();
    new_faucet
}

/// Creates a proven batch from a list of transactions and a reference block.
fn create_batch(txs: &[ProvenTransaction], block_ref: &BlockHeader) -> ProvenBatch {
    let account_updates = txs
        .iter()
        .map(|tx| (tx.account_id(), BatchAccountUpdate::from_transaction(tx)))
        .collect();
    let input_notes = txs.iter().flat_map(|tx| tx.input_notes().iter().cloned()).collect();
    let output_notes = txs.iter().flat_map(|tx| tx.output_notes().iter().cloned()).collect();
    ProvenBatch::new_unchecked(
        BatchId::from_transactions(txs.iter()),
        block_ref.commitment(),
        block_ref.block_num(),
        account_updates,
        InputNotes::new(input_notes).unwrap(),
        output_notes,
        BlockNumber::from(u32::MAX),
        txs.iter().map(TransactionHeader::from).collect(),
    )
}

/// For each pair of account and note, creates a transaction that consumes the note.
fn create_consume_note_txs(
    block_ref: &BlockHeader,
    accounts: Vec<Account>,
    notes: Vec<Note>,
    note_proofs: &BTreeMap<NoteId, NoteInclusionProof>,
) -> Vec<ProvenTransaction> {
    accounts
        .into_iter()
        .zip(notes)
        .map(|(mut account, note)| {
            let inclusion_proof = note_proofs.get(&note.id()).unwrap();
            create_consume_note_tx(
                block_ref,
                &mut account,
                InputNote::authenticated(note, inclusion_proof.clone()),
            )
        })
        .collect()
}

/// Creates a transaction that creates an account and consumes the given input note.
///
/// The account is updated with the assets from the input note, and the nonce is set to 1.
fn create_consume_note_tx(
    block_ref: &BlockHeader,
    account: &mut Account,
    input_note: InputNote,
) -> ProvenTransaction {
    let init_hash = account.init_commitment();

    input_note.note().assets().iter().for_each(|asset| {
        account.vault_mut().add_asset(*asset).unwrap();
    });
    account.set_nonce(Felt::ONE).unwrap();

    ProvenTransactionBuilder::new(
        account.id(),
        init_hash,
        account.commitment(),
        block_ref.block_num(),
        block_ref.commitment(),
        u32::MAX.into(),
        ExecutionProof::new(Proof::new_dummy(), HashFunction::default()),
    )
    .add_input_notes(vec![input_note])
    .build()
    .unwrap()
}

/// Creates a transaction from the faucet that creates the given output notes.
/// Updates the faucet account to increase the issuance slot and it's nonce.
fn create_emit_note_tx(
    block_ref: &BlockHeader,
    faucet: &mut Account,
    output_notes: Vec<Note>,
) -> ProvenTransaction {
    let initial_account_hash = faucet.commitment();

    let slot = faucet.storage().get_item(2).unwrap();
    faucet
        .storage_mut()
        .set_item(0, [slot[0], slot[1], slot[2], slot[3] + Felt::new(10)])
        .unwrap();
    faucet.set_nonce(faucet.nonce() + Felt::ONE).unwrap();

    ProvenTransactionBuilder::new(
        faucet.id(),
        initial_account_hash,
        faucet.commitment(),
        block_ref.block_num(),
        block_ref.commitment(),
        u32::MAX.into(),
        ExecutionProof::new(Proof::new_dummy(), HashFunction::default()),
    )
    .add_output_notes(output_notes.into_iter().map(OutputNote::Full).collect::<Vec<OutputNote>>())
    .build()
    .unwrap()
}

/// Gets the batch inputs from the store and tracks the query time on the metrics.
async fn get_batch_inputs(
    store_client: &StoreClient,
    block_ref: &BlockHeader,
    notes: &[Note],
    metrics: &mut Metrics,
) -> BatchInputs {
    let start = Instant::now();
    // Mark every note as unauthenticated, so that the store returns the inclusion proofs for all of
    // them
    let batch_inputs = store_client
        .get_batch_inputs(
            vec![(block_ref.block_num(), block_ref.commitment())].into_iter(),
            notes.iter().map(Note::id),
        )
        .await
        .unwrap();
    metrics.add_get_batch_inputs(start.elapsed());
    batch_inputs
}

/// Gets the block inputs from the store and tracks the query time on the metrics.
async fn get_block_inputs(
    store_client: &StoreClient,
    batches: &[ProvenBatch],
    metrics: &mut Metrics,
) -> BlockInputs {
    let start = Instant::now();
    let inputs = store_client
        .get_block_inputs(
            batches.iter().flat_map(ProvenBatch::updated_accounts),
            batches.iter().flat_map(ProvenBatch::created_nullifiers),
            batches.iter().flat_map(|batch| {
                batch
                    .input_notes()
                    .into_iter()
                    .filter_map(|note| note.header().map(NoteHeader::id))
            }),
            batches.iter().map(ProvenBatch::reference_block_num),
        )
        .await
        .unwrap();
    let get_block_inputs_time = start.elapsed();
    metrics.add_get_block_inputs(get_block_inputs_time);
    inputs
}
