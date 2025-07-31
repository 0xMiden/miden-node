use std::{collections::VecDeque, sync::Arc};

use anyhow::{Context, anyhow};
use miden_lib::{
    account::{
        faucets::BasicFungibleFaucet,
        interface::{AccountInterface, AccountInterfaceError},
    },
    note::create_p2id_note,
};
use miden_objects::{
    AccountError, Felt, NoteError, Word,
    account::{Account, AccountDelta, AccountFile, AccountId, AuthSecretKey, NetworkId},
    assembly::DefaultSourceManager,
    asset::FungibleAsset,
    block::BlockNumber,
    crypto::{
        merkle::{Forest, MmrPeaks, PartialMmr},
        rand::RpoRandomCoin,
    },
    note::Note,
    transaction::{
        ExecutedTransaction, InputNotes, PartialBlockchain, ProvenTransaction, TransactionArgs,
        TransactionId, TransactionWitness,
    },
};
use miden_remote_prover_client::remote_prover::tx_prover::RemoteTransactionProver;
use miden_tx::{
    LocalTransactionProver, ProvingOptions, TransactionExecutor, TransactionExecutorError,
    TransactionProver, TransactionProverError, auth::BasicAuthenticator,
    utils::parse_hex_string_as_word,
};
use rand::{Rng, rng, rngs::StdRng};
use serde::Serialize;
use store::FaucetDataStore;
use tokio::sync::mpsc::Receiver;
use tonic::Code;
use tracing::{error, info, instrument, warn};
use updates::{ClientUpdater, MintUpdate, ResponseSender};
use url::Url;

use crate::{
    rpc_client::{RpcClient, RpcError},
    types::{AssetAmount, NoteType},
};

mod store;
mod updates;

// FAUCET PROVER
// ================================================================================================

/// Represents a transaction prover which can be either local or remote, and is used to prove
/// transactions minted by the faucet.
enum FaucetProver {
    Local(LocalTransactionProver),
    Remote(RemoteTransactionProver),
}

impl FaucetProver {
    /// Creates a new local prover.
    ///
    /// It uses the default proving options.
    fn local() -> Self {
        Self::Local(LocalTransactionProver::new(ProvingOptions::default()))
    }

    /// Creates a new remote prover.
    ///
    /// # Arguments
    ///
    /// * `endpoint` - The endpoint to connect to the remote prover.
    fn remote(endpoint: Url) -> Self {
        Self::Remote(RemoteTransactionProver::new(endpoint))
    }

    async fn prove(
        &self,
        tx: impl Into<TransactionWitness> + Clone,
    ) -> Result<ProvenTransaction, MintError> {
        match self {
            Self::Local(prover) => prover.prove(tx.into()).await,
            Self::Remote(prover) => {
                let proven_tx = prover.prove(tx.clone().into()).await;
                match proven_tx {
                    Ok(proven_tx) => Ok(proven_tx),
                    Err(err) => {
                        warn!("failed to prove transaction with remote prover, falling back to local prover: {}", err);
                        LocalTransactionProver::new(ProvingOptions::default()).prove(tx.into()).await
                    }
                }
            },
        }
        .map_err(MintError::Proving)
    }
}

// FAUCET CLIENT
// ================================================================================================

/// The faucet's account ID and network ID.
///
/// Used as a type safety mechanism to avoid confusion with user account IDs, and allows us to
/// implement traits.
#[derive(Clone, Copy)]
pub struct FaucetId {
    pub account_id: AccountId,
    pub network_id: NetworkId,
}

impl FaucetId {
    pub fn new(account_id: AccountId, network_id: NetworkId) -> Self {
        Self { account_id, network_id }
    }
}

impl Serialize for FaucetId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.account_id.to_bech32(self.network_id))
    }
}

/// A request for minting to the [`Faucet`].
pub struct MintRequest {
    /// Destination account.
    pub account_id: AccountId,
    /// Whether to generate a public or private note to hold the minted asset.
    pub note_type: NoteType,
    /// The amount to mint.
    pub asset_amount: AssetAmount,
}

type MintResult<T> = Result<T, MintError>;

/// Error indicating what went wrong in the minting process for a request.
#[derive(Debug, thiserror::Error)]
pub enum MintError {
    #[error("compiling the tx script failed")]
    ScriptCompilation(#[source] AccountInterfaceError),
    #[error("execution of the tx script failed")]
    Execution(#[source] TransactionExecutorError),
    #[error("proving the tx failed")]
    Proving(#[source] TransactionProverError),
    #[error("submitting the tx to the node failed")]
    Submission(#[source] RpcError),
    #[error("failed to build notes")]
    NoteBuild(#[source] NoteError),
}

/// Stores the current faucet state and handles minting requests.
pub struct Faucet {
    data_store: Arc<FaucetDataStore>,
    id: FaucetId,
    // Previous faucet account states used to perform rollbacks if a desync is detected.
    prior_state: VecDeque<Account>,
    tx_prover: Arc<FaucetProver>,
    authenticator: BasicAuthenticator<StdRng>,
    account_interface: AccountInterface,
    decimals: u8,
}

impl Faucet {
    /// Loads the faucet state from the node and the account file.
    #[instrument(name = "faucet.load", fields(id), skip_all)]
    pub async fn load(
        network_id: NetworkId,
        account_file: AccountFile,
        rpc_client: &mut RpcClient,
        remote_tx_prover_url: Option<Url>,
    ) -> anyhow::Result<Self> {
        let id = account_file.account.id();
        let id = FaucetId::new(id, network_id);

        tracing::Span::current().record("id", id.account_id.to_string());

        info!("Fetching faucet state from node");

        let account = match rpc_client.get_faucet_account(id).await {
            Ok(account) => {
                info!(
                    commitment = %account.commitment(),
                    nonce = %account.nonce(),
                    "Received faucet account state from the node",
                );

                Ok(account)
            },
            Err(RpcError::Transport(status)) if status.code() == Code::NotFound => {
                let account = account_file.account;
                info!(
                    commitment = %account.commitment(),
                    nonce = %account.nonce(),
                    "Faucet not found in the node, using state from file"
                );

                Ok(account)
            },
            Err(err) => Err(err),
        }
        .context("fetching faucet state from node")?;

        info!("Fetching genesis header from the node");
        let genesis_header = rpc_client
            .get_genesis_header()
            .await
            .context("fetching genesis header from the node")?;

        // SAFETY: An empty Partial Blockchain should be valid.
        let genesis_chain_mmr = PartialBlockchain::new(
            PartialMmr::from_peaks(
                MmrPeaks::new(Forest::empty(), Vec::new()).expect("Empty MmrPeak should be valid"),
            ),
            Vec::new(),
        )
        .expect("Empty ChainMmr should be valid");

        let faucet = BasicFungibleFaucet::try_from(&account)?;
        let account_interface = AccountInterface::from(&account);

        let data_store = Arc::new(FaucetDataStore::new(
            account,
            account_file.account_seed,
            genesis_header,
            genesis_chain_mmr,
        ));

        // We expect exactly one secret key. Anything else implies a bug elsewhere.
        let [keypair] = account_file.auth_secret_keys.as_slice() else {
            anyhow::bail!(
                "Account file must contain exactly 1 authentication key, but found {}",
                account_file.auth_secret_keys.len()
            );
        };
        let keypair = match keypair {
            AuthSecretKey::RpoFalcon512(falcon) => (falcon.public_key().into(), keypair.clone()),
        };
        let authenticator = BasicAuthenticator::<StdRng>::new(&[keypair]);

        let tx_prover = match remote_tx_prover_url {
            Some(url) => Arc::new(FaucetProver::remote(url)),
            None => Arc::new(FaucetProver::local()),
        };

        Ok(Self {
            data_store,
            id,
            prior_state: VecDeque::new(),
            tx_prover,
            account_interface,
            decimals: faucet.decimals(),
            authenticator,
        })
    }

    /// Runs the faucet minting process until the request source is closed, or it encounters a fatal
    /// error.
    pub async fn run(
        mut self,
        mut rpc_client: RpcClient,
        mut requests: Receiver<(MintRequest, ResponseSender)>,
    ) -> anyhow::Result<()> {
        let mut buffer = Vec::new();
        let limit = 100; // we could include 256 notes per tx, but requests channel is limited to 100 atm

        while requests.recv_many(&mut buffer, limit).await > 0 {
            // Skip requests where the user no longer cares about the result.
            let (requests, response_senders): (Vec<MintRequest>, Vec<ResponseSender>) = buffer
                .drain(..)
                .filter(|(request, response_sender)| {
                    if response_sender.is_closed() {
                        tracing::info!(request.account_id=%request.account_id, "request cancelled");
                        false
                    } else {
                        true
                    }
                })
                .unzip();

            let updater = ClientUpdater::new(response_senders, self.id.network_id);

            match self.handle_request_batch(&requests, &mut rpc_client, &updater).await {
                // Update local state on success.
                Ok((delta, block_number, notes, tx_id)) => {
                    updater.send_notes(block_number, &notes, tx_id).await;
                    // SAFETY: Delta must be valid since it comes from a tx accepted by the node.
                    self.update_state(&delta).unwrap();
                },
                // Handle errors if possible, otherwise bail and let the restart handle it.
                Err(err) => {
                    self.error_recovery(err)
                        .context("failed to recover from minting error")
                        .inspect_err(|err| tracing::error!(%err, "minting request failed"))?;
                },
            }
        }

        tracing::info!("Request stream closed, shutting down minter");

        Ok(())
    }

    /// Updates the state of the faucet account, storing the current state for potential rollbacks.
    ///
    /// # Errors
    ///
    /// Follows the same error reasoning as [`Account::apply_delta`].
    fn update_state(&mut self, delta: &AccountDelta) -> Result<(), AccountError> {
        // Store the last 1000 states for rollback purposes.
        if self.prior_state.len() > 1000 {
            self.prior_state.pop_front();
        }
        self.prior_state.push_back(self.data_store.faucet_account());

        let mut account = self.data_store.faucet_account();
        account.apply_delta(delta)?;
        self.data_store.update_faucet_state(account);

        Ok(())
    }

    /// Attempt to recover from errors.
    ///
    /// Notably this includes rolling back local state if a desync occurs.
    ///
    /// Returns an error if recovery was not possible, which should be considered fatal.
    fn error_recovery(&mut self, err: MintError) -> anyhow::Result<()> {
        match err {
            // A state mismatch means we desync'd from the actual chain state, and should resync.
            //
            // This can occur if the node restarts (dropping inflight txs), or if inflight txs got
            // dropped.
            MintError::Submission(RpcError::Transport(err))
                if err.code() == tonic::Code::InvalidArgument
                    && err.message().contains("incorrect initial state commitment") =>
            {
                self.handle_desync(err.message()).with_context(|| {
                    format!("failed to recover from desync error: {}", err.message())
                })
            },
            // TODO: Look into which other errors should be recoverable.
            //       e.g. Connection error being lost to RPC client is probably not fatal.
            others => Err(others).context("failed to handle error"),
        }
    }

    /// Attempts to rollback back local state to match that indicated by the node.
    ///
    /// This relies on parsing the stringified error
    /// `VerifyTxError::IncorrectInitialAccountCommitment`.
    ///
    /// Returns an error if the rollback was unsuccessful. This should be treated as fatal.
    fn handle_desync(&mut self, err: &str) -> anyhow::Result<()> {
        let onchain_state = parse_desync_error(err).context("failed to parse desync message")?;

        // Find the matching local state, unless we've dropped it already.
        let rollback = self
            .prior_state
            .iter()
            .position(|state| state.commitment() == onchain_state)
            .context("no matching local state to rollback to")?;

        // Rollback the local state.
        // SAFETY: The index must exist since the element was just found.
        self.data_store.update_faucet_state(self.prior_state[rollback].clone());
        self.prior_state.drain(rollback..);

        tracing::warn!(rollback.count = rollback, "desync detected and handled");

        Ok(())
    }

    /// Fully handles a batch of requests _without_ changing local state.
    ///
    /// Caller should update the local state based on the returned result.
    async fn handle_request_batch(
        &self,
        requests: &[MintRequest],
        rpc_client: &mut RpcClient,
        updater: &ClientUpdater,
    ) -> MintResult<(AccountDelta, BlockNumber, Vec<Note>, TransactionId)> {
        let mut thread_rng = rng();
        let coin_seed: [u64; 4] = thread_rng.random();

        let mut rng = RpoRandomCoin::new(coin_seed.map(Felt::new).into());

        let p2id_notes = P2IdNotes::build(self.faucet_id(), self.decimals, requests, &mut rng)?;

        // Build the note
        let notes = p2id_notes.into_inner();
        let tx_args = self.compile(&notes)?;

        self.data_store
            .load_transaction_script(tx_args.tx_script().expect("should have script"));

        updater.send_updates(MintUpdate::Built).await;

        // Execute the transaction
        let executed_transaction = self.execute_transaction(tx_args).await?;
        let account_delta = executed_transaction.account_delta().clone();
        let tx_id = executed_transaction.id();
        updater.send_updates(MintUpdate::Executed).await;

        // Prove the transaction
        let tx = self.tx_prover.as_ref().prove(executed_transaction).await?;
        updater.send_updates(MintUpdate::Proven).await;

        // Submit the transaction
        let block_number = self.submit_transaction(tx, rpc_client).await?;
        updater.send_updates(MintUpdate::Submitted).await;

        Ok((account_delta, block_number, notes, tx_id))
    }

    /// Compiles the transaction script that creates the given set of notes.
    fn compile(&self, notes: &[Note]) -> MintResult<TransactionArgs> {
        let partial_notes = notes.iter().map(Into::into).collect::<Vec<_>>();
        let script = self
            .account_interface
            .build_send_notes_script(&partial_notes, None, false)
            .map_err(MintError::ScriptCompilation)?;

        let mut transaction_args = TransactionArgs::default().with_tx_script(script);
        transaction_args.extend_output_note_recipients(notes);

        Ok(transaction_args)
    }

    async fn execute_transaction(
        &self,
        tx_args: TransactionArgs,
    ) -> MintResult<ExecutedTransaction> {
        let executor =
            TransactionExecutor::new(self.data_store.as_ref(), Some(&self.authenticator));
        executor
            .execute_transaction(
                self.id.account_id,
                BlockNumber::GENESIS,
                InputNotes::default(),
                tx_args,
                Arc::new(DefaultSourceManager::default()),
            )
            .await
            .map_err(MintError::Execution)
    }

    async fn submit_transaction(
        &self,
        tx: ProvenTransaction,
        rpc_client: &mut RpcClient,
    ) -> MintResult<BlockNumber> {
        rpc_client.submit_transaction(tx).await.map_err(MintError::Submission)
    }

    /// Returns the id of the faucet account.
    pub fn faucet_id(&self) -> FaucetId {
        self.id
    }
}

// HELPER FUNCTIONS
// ================================================================================================

fn parse_desync_error(err: &str) -> Result<Word, anyhow::Error> {
    let onchain_state = err
        .split_once("current value of ")
        .map(|(_prefix, suffix)| suffix)
        .and_then(|suffix| suffix.split_whitespace().next())
        .context("failed to find current commitment")?;

    // This is used to represent the empty account state.
    if onchain_state.eq_ignore_ascii_case("none") {
        return Ok(Word::empty());
    }

    parse_hex_string_as_word(onchain_state)
        .map_err(|err| anyhow!("failed to parse expected commitment {onchain_state}: {err}"))
        .map(Into::into)
}

/// A collection of `P2Id` notes.
struct P2IdNotes(Vec<Note>);

impl P2IdNotes {
    /// Builds a collection of `P2Id` notes from a set of mint requests.
    ///
    /// # Errors
    ///
    /// Returns an error if creating any p2id note fails.
    fn build(
        source: FaucetId,
        decimals: u8,
        requests: &[MintRequest],
        rng: &mut RpoRandomCoin,
    ) -> Result<Self, MintError> {
        // If building a note fails, we discard the whole batch. Should never happen, since account
        // ids are validated on the request level.
        let mut notes = Vec::new();
        for request in requests {
            let amount = request.asset_amount.inner() * 10u64.pow(decimals.into());
            // SAFETY: source is definitely a faucet account, and the amount is valid.
            let asset = FungibleAsset::new(source.account_id, amount).unwrap();
            let note = create_p2id_note(
                source.account_id,
                request.account_id,
                vec![asset.into()],
                request.note_type.into(),
                Felt::default(),
                rng,
            ).inspect_err(|err| tracing::error!(request.account_id=%request.account_id, ?err, "failed to build note")).map_err(MintError::NoteBuild)?;
            notes.push(note);
        }
        Ok(Self(notes))
    }

    fn into_inner(self) -> Vec<Note> {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use std::{str::FromStr, sync::Mutex, time::Duration};

    use miden_lib::{AuthScheme, account::faucets::create_basic_fungible_faucet};
    use miden_node_block_producer::errors::{AddTransactionError, VerifyTxError};
    use miden_node_utils::crypto::get_rpo_random_coin;
    use miden_objects::{
        Word,
        account::{AccountIdVersion, AccountStorageMode, AccountType},
        asset::TokenSymbol,
        crypto::dsa::rpo_falcon512::SecretKey,
    };
    use rand::{Rng, SeedableRng};
    use rand_chacha::ChaCha20Rng;
    use tokio::time::{Instant, sleep};
    use url::Url;

    use super::*;
    use crate::{stub_rpc_api::serve_stub, types::AssetOptions};

    /// This test ensures that the we are able to parse account mismatch errors
    /// provided by the block-producer.
    ///
    /// This test isn't fully secure as there is still an RPC component and gRPC
    /// infrastructure in the way.
    #[test]
    fn desync_error_parsing() {
        // TODO: This would be better as an integration test.
        let tx_state = Word::from([0u32, 1, 2, 3]);
        let actual = Word::from([11u32, 12, 13, 14]);
        let err = AddTransactionError::VerificationFailed(
            VerifyTxError::IncorrectAccountInitialCommitment {
                tx_initial_account_commitment: tx_state,
                current_account_commitment: Some(actual),
            },
        );
        let err = tonic::Status::from(err);
        let result = parse_desync_error(dbg!(err.message())).unwrap();

        assert_eq!(result, actual);
    }

    // This test ensures that the faucet can create a transaction that outputs a batch of notes.
    #[tokio::test]
    async fn faucet_batches_requests() {
        let stub_node_url = Url::from_str("http://localhost:50052").unwrap();
        // SAFETY: The stub_node_url is a hardcoded test URL that is known to be valid.
        let mut rpc_client = RpcClient::connect_lazy(&stub_node_url, 1000).unwrap();

        // Start the stub node
        tokio::spawn(async move { serve_stub(&stub_node_url).await.unwrap() });

        // Wait for the stub node to serve requests
        let start = Instant::now();
        while rpc_client.get_genesis_header().await.is_err() {
            sleep(Duration::from_millis(100)).await;
            assert!(start.elapsed() < Duration::from_secs(5), "stub node took too long to start");
        }

        // Create the faucet
        let faucet = {
            let mut rng = ChaCha20Rng::from_seed(rand::random());
            let secret = SecretKey::with_rng(&mut get_rpo_random_coin(&mut rng));
            let (account, account_seed) = create_basic_fungible_faucet(
                rng.random(),
                TokenSymbol::try_from("POL").unwrap(),
                2,
                Felt::try_from(1_000_000_000_000u64).unwrap(),
                AccountStorageMode::Public,
                AuthScheme::RpoFalcon512 { pub_key: secret.public_key() },
            )
            .unwrap();
            let account_file = AccountFile::new(
                account,
                Some(account_seed),
                vec![AuthSecretKey::RpoFalcon512(secret)],
            );

            Faucet::load(NetworkId::Testnet, account_file, &mut rpc_client, None)
                .await
                .unwrap()
        };

        // Create a set of mint requests
        let num_requests = 5;
        let requests = (0..num_requests)
            .map(|i| {
                let account_id = AccountId::dummy(
                    [i; 15],
                    AccountIdVersion::Version0,
                    AccountType::RegularAccountImmutableCode,
                    AccountStorageMode::Private,
                );
                MintRequest {
                    account_id,
                    asset_amount: AssetOptions::new(vec![100]).unwrap().validate(100).unwrap(),
                    note_type: NoteType::Public,
                }
            })
            .collect::<Vec<_>>();

        let coin_seed: [u64; 4] = rand::rng().random();
        let rng = Arc::new(Mutex::new(RpoRandomCoin::new(coin_seed.map(Felt::new).into())));
        let mut rng = *rng.lock().unwrap();

        // Build and execute the transaction
        let notes = P2IdNotes::build(faucet.faucet_id(), 6, &requests, &mut rng)
            .unwrap()
            .into_inner();
        let tx_args = faucet.compile(&notes).unwrap();

        faucet
            .data_store
            .load_transaction_script(tx_args.tx_script().expect("should have script"));

        let executed_tx = faucet.execute_transaction(tx_args).await.unwrap();

        assert_eq!(executed_tx.output_notes().num_notes(), num_requests as usize);
        assert_eq!(notes.len(), num_requests as usize);
    }
}
