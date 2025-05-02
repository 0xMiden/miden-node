use std::{collections::VecDeque, sync::Arc, time::Duration};

use anyhow::{Context, anyhow};
use miden_lib::{note::create_p2id_note, transaction::TransactionKernel};
use miden_node_proto::generated::{
    requests::{
        GetAccountDetailsRequest, GetBlockHeaderByNumberRequest, SubmitProvenTransactionRequest,
    },
    rpc::api_client::ApiClient,
};
use miden_objects::{
    AccountError, Digest, Felt, NoteError, TransactionScriptError,
    account::{Account, AccountDelta, AccountFile, AccountId, AuthSecretKey},
    asset::FungibleAsset,
    block::{BlockHeader, BlockNumber},
    crypto::{
        merkle::{MmrPeaks, PartialMmr},
        rand::{FeltRng, RpoRandomCoin},
    },
    note::Note,
    transaction::{
        ChainMmr, ExecutedTransaction, ProvenTransaction, TransactionArgs, TransactionScript,
    },
    utils::Deserializable,
    vm::AdviceMap,
};
use miden_tx::{
    LocalTransactionProver, ProvingOptions, TransactionExecutor, TransactionExecutorError,
    TransactionProver, TransactionProverError,
    auth::BasicAuthenticator,
    utils::{Serializable, parse_hex_string_as_word},
};
use rand::{random, rngs::StdRng};
use serde::Serialize;
use tokio::sync::{mpsc, oneshot};
use tonic::transport::Channel;
use tracing::info;

use crate::{
    COMPONENT,
    config::FaucetConfig,
    store::FaucetDataStore,
    types::{AssetAmount, NoteType},
};

pub const DISTRIBUTE_FUNGIBLE_ASSET_SCRIPT: &str =
    include_str!("transaction_scripts/distribute_fungible_asset.masm");

// FAUCET CLIENT
// ================================================================================================

/// The faucet's account ID.
///
/// Used as a type safety mechanism to avoid confusion with user account IDs,
/// and allows us to implement traits.
#[derive(Clone, Copy)]
pub struct FaucetId(AccountId);

impl FaucetId {
    fn inner(self) -> AccountId {
        self.0
    }
}

impl Serialize for FaucetId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0.to_hex())
    }
}

/// A request for minting to the [`FaucetClient`].
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
///
/// Unfortunately the inner errors do not implement Clone, so broader interactions
/// with this type are usually wrapped in an Arc.
#[derive(Debug, thiserror::Error)]
pub enum MintError {
    #[error("building the p2id note failed")]
    BuildingP2IdNote(NoteError),
    #[error("compiling the tx script failed")]
    ScriptCompilation(TransactionScriptError),
    #[error("execution of the tx script failed")]
    Execution(TransactionExecutorError),
    #[error("proving the tx failed")]
    Proving(TransactionProverError),
    #[error("submitting the tx to the node failed")]
    Submission(tonic::Status),
}

/// Basic client that handles execution, proving and submitting of mint transactions
/// for the faucet.
pub struct FaucetClient {
    executor: TransactionExecutor,
    data_store: Arc<FaucetDataStore>,
    id: FaucetId,
    // Previous faucet account states used to perform rollbacks if a desync is detected.
    prior_state: VecDeque<Account>,
}

// TODO: Remove this once https://github.com/0xPolygonMiden/miden-base/issues/909 is resolved
unsafe impl Send for FaucetClient {}

impl FaucetClient {
    /// Fetches the latest faucet account state from the node and creates a new faucet client.
    ///
    /// # Note
    /// If the faucet account is not found on chain, it will be created on submission of the first
    /// minting transaction.
    pub async fn new(config: &FaucetConfig) -> anyhow::Result<Self> {
        let (mut rpc_api, root_block_header, root_chain_mmr) =
            initialize_faucet_client(config).await?;

        let faucet_account_data = AccountFile::read(&config.faucet_account_path)
            .context("failed to load faucet account from file")?;

        let id = faucet_account_data.account.id();
        let id = FaucetId(id);

        info!(target: COMPONENT, "Requesting account state from the node...");
        let faucet_account = match request_account_state(&mut rpc_api, id.inner()).await {
            Ok(account) => {
                info!(
                    target: COMPONENT,
                    commitment = %account.commitment(),
                    nonce = %account.nonce(),
                    "Received faucet account state from the node",
                );

                account
            },

            Err(err) => match err {
                ClientError::RequestError(status) if status.code() == tonic::Code::NotFound => {
                    info!(target: COMPONENT, "Faucet account not found in the node");

                    faucet_account_data.account
                },
                _ => {
                    return Err(err);
                },
            },
        };

        let data_store = Arc::new(FaucetDataStore::new(
            faucet_account,
            faucet_account_data.account_seed,
            root_block_header,
            root_chain_mmr,
        ));

        let public_key = match &faucet_account_data.auth_secret_key {
            AuthSecretKey::RpoFalcon512(secret) => secret.public_key(),
        };

        let authenticator = BasicAuthenticator::<StdRng>::new(&[(
            public_key.into(),
            faucet_account_data.auth_secret_key,
        )]);

        let executor = TransactionExecutor::new(data_store.clone(), Some(Arc::new(authenticator)));

        Ok(Self {
            executor,
            data_store,
            id,
            prior_state: VecDeque::new(),
        })
    }

    /// Runs the faucet minting process until the request source is closed, or it encounters a fatal error.
    pub async fn run(
        mut self,
        mut rpc_client: ApiClient<Channel>,
        mut requests: mpsc::Receiver<(
            MintRequest,
            oneshot::Sender<Result<(BlockNumber, Note), Arc<MintError>>>,
        )>,
    ) -> anyhow::Result<()> {
        let coin_seed: [u64; 4] = random();
        let rng = RpoRandomCoin::new(coin_seed.map(Felt::new));

        while let Some((request, response_sender)) = requests.recv().await {
            // Skip doing work if the user no longer cares about the result.
            if response_sender.is_closed() {
                tracing::info!(request.account_id=%request.account_id, "request cancelled");
                continue;
            }

            // Update local state on success.
            let result = self
                .handle_request(request, rng, &mut rpc_client)
                .await
                // SAFETY: Account delta must be valid since the tx was executed, proven and accepted by the node.
                .inspect(|(delta, ..)| self.update_state(delta).unwrap())
                .map(|(_, block_height, note)| (block_height, note))
                // Error isn't clone :(
                .map_err(Arc::new);

            // We ignore the channel closure here as the user may have cancelled the request.
            let _ = response_sender.send(result.clone());

            // Handle errors if possible, otherwise bail and let the restart handle it.
            if let Err(err) = result {
                self.error_recovery(&err).await?;
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
    async fn error_recovery(&mut self, err: &MintError) -> anyhow::Result<()> {
        match err {
            // A state mismatch means we desync'd from the actual chain state, and should resync.
            //
            // This can occur if the node restarts (dropping inflight txs), or if inflight txs got dropped.
            MintError::Submission(err)
                if err.code() == tonic::Code::InvalidArgument
                    && err.message().contains("incorrect initial state commitment") =>
            {
                self.handle_desync(err.message()).with_context(|| {
                    format!("failed to recover from desync error: {}", err.message())
                })
            },
            // TODO: Look into which other errors should be recoverable.
            //       e.g. Connection error to RPC client is probably not fatal.
            others => Err(others).context("failed to handle error"),
        }
    }

    /// Attempts to rollback back local state to match that indicated by the node.
    ///
    /// This relies on parsing the stringified error `VerifyTxError::IncorrectInitialAccountCommitment`.
    ///
    /// Returns an error if the rollback was unsuccesful. This should be treated as fatal.
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

    /// Fully handles a single request _without_ changing local state.
    ///
    /// Caller should update the local state based on the returned result.
    async fn handle_request(
        &self,
        request: MintRequest,
        rng: impl FeltRng,
        rpc_client: &mut ApiClient<Channel>,
    ) -> MintResult<(AccountDelta, BlockNumber, Note)> {
        // Generate the payment note and compile it into our transaction arguments.
        let p2id_note = P2IdNote::build(self.faucet_id(), &request, rng)?;
        let tx_args = p2id_note.compile()?;

        let tx = self.execute_transaction(tx_args)?;
        let account_delta = tx.account_delta().clone();

        let tx = Self::prove_transaction(tx)?;
        let block_height = self.submit_transaction(tx, rpc_client).await?;

        Ok((account_delta, block_height, p2id_note.into_inner()))
    }

    fn execute_transaction(&self, tx_args: TransactionArgs) -> MintResult<ExecutedTransaction> {
        self.executor
            .execute_transaction(self.id.inner(), BlockNumber::GENESIS, &[], tx_args)
            .map_err(MintError::Execution)
    }

    fn prove_transaction(tx: ExecutedTransaction) -> MintResult<ProvenTransaction> {
        LocalTransactionProver::new(ProvingOptions::default())
            .prove(tx.into())
            .map_err(MintError::Proving)
    }

    async fn submit_transaction(
        &self,
        tx: ProvenTransaction,
        rpc_client: &mut ApiClient<Channel>,
    ) -> MintResult<BlockNumber> {
        let request = SubmitProvenTransactionRequest { transaction: tx.to_bytes() };

        rpc_client
            .submit_proven_transaction(request)
            .await
            .map(|response| response.into_inner().block_height.into())
            .map_err(MintError::Submission)
    }

    /// Returns the id of the faucet account.
    pub fn faucet_id(&self) -> FaucetId {
        self.id
    }
}

// HELPER FUNCTIONS
// ================================================================================================

fn parse_desync_error(err: &str) -> Result<Digest, anyhow::Error> {
    let onchain_state = err
        .split_once("current:")
        .map(|(_prefix, suffix)| suffix)
        .and_then(|suffix| suffix.split_whitespace().next())
        .context("failed to find expected commitment")?;

    // This is used to represent the empty account state.
    if onchain_state.eq_ignore_ascii_case("none") {
        return Ok(Digest::default());
    }

    parse_hex_string_as_word(onchain_state)
        .map_err(|err| anyhow!("failed to parse expected commitment: {err}"))
        .map(Into::into)
}

/// Initializes the faucet client by connecting to the node and fetching the root block header.
pub async fn initialize_faucet_client(
    config: &FaucetConfig,
) -> Result<(ApiClient<Channel>, BlockHeader, ChainMmr), ClientError> {
    let endpoint = tonic::transport::Endpoint::try_from(config.node_url.to_string())
        .context("Failed to parse node URL from configuration file")?
        .timeout(Duration::from_millis(config.timeout_ms));

    let mut rpc_api =
        ApiClient::connect(endpoint).await.context("Failed to connect to the node")?;

    let request = GetBlockHeaderByNumberRequest {
        block_num: Some(0),
        include_mmr_proof: None,
    };
    let response = rpc_api
        .get_block_header_by_number(request)
        .await
        .context("Failed to get block header")?;
    let root_block_header = response
        .into_inner()
        .block_header
        .context("Missing root block header in response")?;

    let root_block_header = root_block_header.try_into().context("Failed to parse block header")?;

    let root_chain_mmr = ChainMmr::new(
        PartialMmr::from_peaks(
            MmrPeaks::new(0, Vec::new()).expect("Empty MmrPeak should be valid"),
        ),
        Vec::new(),
    )
    .expect("Empty ChainMmr should be valid");

    Ok((rpc_api, root_block_header, root_chain_mmr))
}

/// Requests account state from the node.
///
/// The account is expected to be public, otherwise, the error is returned.
async fn request_account_state(
    rpc_api: &mut ApiClient<Channel>,
    account_id: AccountId,
) -> Result<Account, ClientError> {
    let account_info = rpc_api
        .get_account_details(GetAccountDetailsRequest { account_id: Some(account_id.into()) })
        .await?
        .into_inner()
        .details
        .context("Account info field is empty")?;

    let faucet_account_state_bytes =
        account_info.details.context("Account details field is empty")?;

    Account::read_from_bytes(&faucet_account_state_bytes)
        .context("Failed to deserialize faucet account")
        .map_err(Into::into)
}

struct P2IdNote(Note);

impl P2IdNote {
    fn build(source: FaucetId, request: &MintRequest, mut rng: impl FeltRng) -> MintResult<Self> {
        // SAFETY: source is definitely a faucet account, and the amount is valid.
        let asset = FungibleAsset::new(source.inner(), request.asset_amount.inner()).unwrap();

        create_p2id_note(
            source.inner(),
            request.account_id,
            vec![asset.into()],
            request.note_type.into(),
            Felt::default(),
            &mut rng,
        )
        .map_err(MintError::BuildingP2IdNote)
        .map(Self)
    }

    fn compile(&self) -> MintResult<TransactionArgs> {
        let note = &self.0;
        let recipient = note
            .recipient()
            .digest()
            .iter()
            .map(|x| x.as_int().to_string())
            .collect::<Vec<_>>()
            .join(".");

        // SAFETY: Its a P2Id note with a single fungible asset by construction.
        let asset = note.assets().iter().next().unwrap().unwrap_fungible();
        let note_type = note.metadata().note_type();
        let tag = note.metadata().tag().inner();
        let aux = note.metadata().aux().inner();
        let execution_hint = note.metadata().execution_hint().into();

        let script = &DISTRIBUTE_FUNGIBLE_ASSET_SCRIPT
            .replace("{recipient}", &recipient)
            .replace("{note_type}", &Felt::new(note_type as u64).to_string())
            .replace("{aux}", &Felt::new(aux).to_string())
            .replace("{tag}", &Felt::new(tag.into()).to_string())
            .replace("{amount}", &Felt::new(asset.amount()).to_string())
            .replace("{execution_hint}", &Felt::new(execution_hint).to_string());

        // SAFETY: This is a basic p2id note so this should always succeed.
        let script = TransactionScript::compile(script, vec![], TransactionKernel::assembler())
            .map_err(MintError::ScriptCompilation)?;

        let mut transaction_args = TransactionArgs::new(Some(script), None, AdviceMap::new());
        transaction_args.extend_output_note_recipients(vec![note]);

        Ok(transaction_args)
    }

    fn into_inner(self) -> Note {
        self.0
    }
}
