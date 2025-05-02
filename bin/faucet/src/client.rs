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
    Digest, Felt,
    account::{Account, AccountFile, AccountId, AuthSecretKey},
    asset::FungibleAsset,
    block::{BlockHeader, BlockNumber},
    crypto::{
        merkle::{MmrPeaks, PartialMmr},
        rand::RpoRandomCoin,
    },
    note::Note,
    transaction::{
        ChainMmr, ExecutedTransaction, ProvenTransaction, TransactionArgs, TransactionScript,
    },
    utils::Deserializable,
    vm::AdviceMap,
};
use miden_tx::{
    LocalTransactionProver, ProvingOptions, TransactionExecutor, TransactionProver,
    auth::BasicAuthenticator,
    utils::{Serializable, parse_hex_string_as_word},
};
use rand::{random, rngs::StdRng};
use tonic::transport::Channel;
use tracing::info;

use crate::{
    COMPONENT,
    config::FaucetConfig,
    errors::{ClientError, ExecutionError, MintError, MintResult},
    store::FaucetDataStore,
};

pub const DISTRIBUTE_FUNGIBLE_ASSET_SCRIPT: &str =
    include_str!("transaction_scripts/distribute_fungible_asset.masm");

// FAUCET CLIENT
// ================================================================================================

/// A request for minting to the [`FaucetClient`].
pub struct MintRequest {
    /// Destination account.
    pub account_id: AccountId,
    /// Whether to generate a public or private note to hold the minted asset.
    pub note_type: NoteType,
    /// The amount to mint.
    pub asset_amount: u64,
}

/// Type of note to generate for a [`MintRequest`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum NoteType {
    Private,
    Public,
}

impl From<NoteType> for miden_objects::note::NoteType {
    fn from(value: NoteType) -> Self {
        match value {
            NoteType::Private => Self::Private,
            NoteType::Public => Self::Public,
        }
    }
}

/// Basic client that handles execution, proving and submitting of mint transactions
/// for the faucet.
pub struct FaucetClient {
    rpc_api: ApiClient<Channel>,
    executor: TransactionExecutor,
    data_store: Arc<FaucetDataStore>,
    id: AccountId,
    rng: RpoRandomCoin,
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
    pub async fn new(config: &FaucetConfig) -> Result<Self, ClientError> {
        let (mut rpc_api, root_block_header, root_chain_mmr) =
            initialize_faucet_client(config).await?;

        let faucet_account_data = AccountFile::read(&config.faucet_account_path)
            .context("Failed to load faucet account from file")?;

        let id = faucet_account_data.account.id();

        info!(target: COMPONENT, "Requesting account state from the node...");
        let faucet_account = match request_account_state(&mut rpc_api, id).await {
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

        let coin_seed: [u64; 4] = random();
        let rng = RpoRandomCoin::new(coin_seed.map(Felt::new));

        Ok(Self {
            rpc_api,
            executor,
            data_store,
            id,
            rng,
            prior_state: VecDeque::new(),
        })
    }

    /// Runs the faucet minting process until the request source is closed, or it encounters a fatal error.
    pub async fn run(
        mut self,
        mut requests: tokio::sync::mpsc::Receiver<(
            MintRequest,
            tokio::sync::oneshot::Sender<MintResult<(BlockNumber, Note)>>,
        )>,
    ) -> anyhow::Result<()> {
        while let Some((request, response_sender)) = requests.recv().await {
            // Skip doing work if the user no longer cares about the result.
            if response_sender.is_closed() {
                tracing::info!(request.account_id=%request.account_id, "request cancelled");
                continue;
            }

            // Update local state on success.
            let result = self.handle_request(request).await.map(|(state, block_height, note)| {
                // Store the last 1000 states for rollback purposes.
                if self.prior_state.len() > 1000 {
                    self.prior_state.pop_front();
                }
                self.prior_state.push_back(self.data_store.faucet_account());
                self.data_store.update_faucet_state(state);

                (block_height, note)
            });

            // We ignore the channel closure here as the user may have cancelled the request.
            let _ = response_sender.send(result.clone());

            // Handle errors if possible, otherwise bail and let the restart handle it.
            if let Err(err) = result {
                self.error_recovery(err).await?;
            }
        }

        tracing::info!("Request stream closed, shutting down minter");

        Ok(())
    }

    /// Attempt to recover from errors.
    ///
    /// Notably this includes rolling back local state if a desync occurs.
    ///
    /// Returns an error if recovery was not possible, which should be considered fatal.
    async fn error_recovery(&mut self, err: MintError) -> anyhow::Result<()> {
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
    /// Caller should update the faucet account state on a succesful call.
    async fn handle_request(
        &mut self,
        request: MintRequest,
    ) -> MintResult<(Account, BlockNumber, Note)> {
        // info!(target: COMPONENT, "Executing mint transaction for account.");
        let (executed_tx, created_note) = self.execute_mint_transaction(request)?;

        let mut faucet_account = self.data_store.faucet_account();
        faucet_account
            .apply_delta(executed_tx.account_delta())
            .map_err(MintError::Account)?;

        // TODO: error handling.
        let proven_transaction = Self::prove_transaction(executed_tx)?;

        // Run transaction prover & send transaction to node
        // info!(target: COMPONENT, "Proving and submitting transaction.");
        // TODO: enable remote prover.
        // TODO: handle errors and desync failures here.
        let block_height = self.submit_transaction(proven_transaction).await?;

        Ok((faucet_account, block_height, created_note))
    }

    /// Executes a mint transaction for the target account.
    ///
    /// Returns the executed transaction and the expected output note.
    fn execute_mint_transaction(
        &mut self,
        request: MintRequest,
    ) -> MintResult<(ExecutedTransaction, Note)> {
        // SAFETY: self.id is definitely a faucet account, and the amount is valid.
        // TODO: type safety the asset amount as part of request.
        let asset = FungibleAsset::new(self.id, request.asset_amount).unwrap();

        let output_note = create_p2id_note(
            self.id,
            request.account_id,
            vec![asset.into()],
            request.note_type.into(),
            Felt::default(),
            &mut self.rng,
        )
        .map_err(ExecutionError::NoteCreation)?;

        let transaction_args =
            build_transaction_arguments(&output_note, request.note_type.into(), asset)?;

        let executed_tx = self
            .executor
            .execute_transaction(self.id, 0.into(), &[], transaction_args)
            .map_err(ExecutionError::Execution)?;

        Ok((executed_tx, output_note))
    }

    fn prove_transaction(tx: ExecutedTransaction) -> MintResult<ProvenTransaction> {
        LocalTransactionProver::new(ProvingOptions::default())
            .prove(tx.into())
            .map_err(MintError::Proving)
    }

    async fn submit_transaction(&mut self, tx: ProvenTransaction) -> MintResult<BlockNumber> {
        // Prepare request with proven transaction.
        // This is needed to be in a separated code block in order to release reference to avoid
        // borrow checker error.
        let request = SubmitProvenTransactionRequest { transaction: tx.to_bytes() };

        self.rpc_api
            .submit_proven_transaction(request)
            .await
            .map(|response| response.into_inner().block_height.into())
            .map_err(MintError::Submission)
    }

    /// Returns the id of the faucet account.
    pub fn faucet_id(&self) -> AccountId {
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

/// Builds transaction arguments for the mint transaction.
fn build_transaction_arguments(
    output_note: &Note,
    note_type: NoteType,
    asset: FungibleAsset,
) -> Result<TransactionArgs, ExecutionError> {
    let recipient = output_note
        .recipient()
        .digest()
        .iter()
        .map(|x| x.as_int().to_string())
        .collect::<Vec<_>>()
        .join(".");

    let tag = output_note.metadata().tag().inner();
    let aux = output_note.metadata().aux().inner();
    let execution_hint = output_note.metadata().execution_hint().into();

    let script = &DISTRIBUTE_FUNGIBLE_ASSET_SCRIPT
        .replace("{recipient}", &recipient)
        .replace("{note_type}", &Felt::new(note_type as u64).to_string())
        .replace("{aux}", &Felt::new(aux).to_string())
        .replace("{tag}", &Felt::new(tag.into()).to_string())
        .replace("{amount}", &Felt::new(asset.amount()).to_string())
        .replace("{execution_hint}", &Felt::new(execution_hint).to_string());

    let script = TransactionScript::compile(script, vec![], TransactionKernel::assembler())
        .map_err(ExecutionError::TransactionCompilation)?;

    let mut transaction_args = TransactionArgs::new(Some(script), None, AdviceMap::new());
    transaction_args.extend_output_note_recipients(vec![output_note.clone()]);

    Ok(transaction_args)
}
