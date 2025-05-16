use std::{sync::Arc, time::Duration};

use anyhow::Context;
use miden_lib::{note::create_p2id_note, transaction::TransactionKernel};
use miden_node_proto::generated::{
    requests::{
        GetAccountDetailsRequest, GetBlockHeaderByNumberRequest, SubmitProvenTransactionRequest,
    },
    rpc::api_client::ApiClient,
};
use miden_objects::{
    Felt,
    account::{Account, AccountFile, AccountId, AuthSecretKey},
    asset::FungibleAsset,
    block::{BlockHeader, BlockNumber},
    crypto::{
        merkle::{MmrPeaks, PartialMmr},
        rand::RpoRandomCoin,
    },
    note::{Note, NoteType},
    transaction::{
        AccountInputs, ExecutedTransaction, InputNote, InputNotes, PartialBlockchain,
        TransactionArgs, TransactionScript,
    },
    utils::Deserializable,
    vm::AdviceMap,
};
use miden_tx::{
    LocalTransactionProver, ProvingOptions, TransactionExecutor, TransactionProver,
    auth::BasicAuthenticator, utils::Serializable,
};
use rand::{random, rngs::StdRng};
use tonic::transport::Channel;
use tracing::{info, instrument};

use crate::{COMPONENT, config::FaucetConfig, errors::ClientError, store::FaucetDataStore};

pub const DISTRIBUTE_FUNGIBLE_ASSET_SCRIPT: &str =
    include_str!("transaction_scripts/distribute_fungible_asset.masm");

// FAUCET CLIENT
// ================================================================================================

/// Basic client that handles execution, proving and submitting of mint transactions
/// for the faucet.
pub struct FaucetClient {
    rpc_api: ApiClient<Channel>,
    executor: TransactionExecutor,
    data_store: Arc<FaucetDataStore>,
    id: AccountId,
    rng: RpoRandomCoin,
}

// TODO: Remove this once https://github.com/0xMiden/miden-base/issues/909 is resolved
unsafe impl Send for FaucetClient {}

impl FaucetClient {
    /// Fetches the latest faucet account state from the node and creates a new faucet client.
    ///
    /// # Note
    /// If the faucet account is not found on chain, it will be created on submission of the first
    /// minting transaction.
    pub async fn new(config: &FaucetConfig) -> Result<Self, ClientError> {
        let (mut rpc_api, root_block_header, root_partial_block_chain) =
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
            root_partial_block_chain,
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

        Ok(Self { rpc_api, executor, data_store, id, rng })
    }

    /// Executes a mint transaction for the target account.
    ///
    /// Returns the executed transaction and the expected output note.
    #[instrument(target = COMPONENT, name = "faucet.client.execute_mint_transaction", skip_all, err)]
    pub fn execute_mint_transaction(
        &mut self,
        target_account_id: AccountId,
        is_private_note: bool,
        asset_amount: u64,
    ) -> Result<(ExecutedTransaction, Note), ClientError> {
        let asset =
            FungibleAsset::new(self.id, asset_amount).context("Failed to create fungible asset")?;

        let note_type = if is_private_note {
            NoteType::Private
        } else {
            NoteType::Public
        };

        let output_note = create_p2id_note(
            self.id,
            target_account_id,
            vec![asset.into()],
            note_type,
            Felt::default(),
            &mut self.rng,
        )
        .context("Failed to create P2ID note")?;

        let transaction_args = build_transaction_arguments(&output_note, note_type, asset)?;
        self.data_store
            .load_transaction_script(transaction_args.tx_script().expect("should have script"));

        let executed_tx = self
            .executor
            .execute_transaction(
                self.id,
                0.into(),
                InputNotes::<InputNote>::default(),
                transaction_args,
            )
            .context("Failed to execute transaction")?;

        Ok((executed_tx, output_note))
    }

    /// Proves and submits the executed transaction to the node.
    #[instrument(target = COMPONENT, name = "faucet.client.prove_and_submit_transaction", skip_all, err)]
    pub async fn prove_and_submit_transaction(
        &mut self,
        executed_tx: ExecutedTransaction,
    ) -> Result<BlockNumber, ClientError> {
        // Prepare request with proven transaction.
        // This is needed to be in a separated code block in order to release reference to avoid
        // borrow checker error.
        let request = {
            let transaction_prover = LocalTransactionProver::new(ProvingOptions::default());

            let proven_transaction = transaction_prover
                .prove(executed_tx.into())
                .context("Failed to prove transaction")?;

            SubmitProvenTransactionRequest {
                transaction: proven_transaction.to_bytes(),
            }
        };

        let response = self
            .rpc_api
            .submit_proven_transaction(request)
            .await
            .context("Failed to submit proven transaction")?;

        Ok(response.into_inner().block_height.into())
    }

    /// Returns a reference to the data store.
    pub fn data_store(&self) -> &FaucetDataStore {
        &self.data_store
    }

    /// Returns the id of the faucet account.
    pub fn get_faucet_id(&self) -> AccountId {
        self.id
    }
}

// HELPER FUNCTIONS
// ================================================================================================

/// Initializes the faucet client by connecting to the node and fetching the root block header.
pub async fn initialize_faucet_client(
    config: &FaucetConfig,
) -> Result<(ApiClient<Channel>, BlockHeader, PartialBlockchain), ClientError> {
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

    let root_partial_block_chain = PartialBlockchain::new(
        PartialMmr::from_peaks(
            MmrPeaks::new(0, Vec::new()).expect("Empty MmrPeak should be valid"),
        ),
        Vec::new(),
    )
    .expect("Empty PartialBlockchain should be valid");

    Ok((rpc_api, root_block_header, root_partial_block_chain))
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
#[allow(clippy::result_large_err)]
fn build_transaction_arguments(
    output_note: &Note,
    note_type: NoteType,
    asset: FungibleAsset,
) -> Result<TransactionArgs, ClientError> {
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
        .context("Failed to compile script")?;

    let mut transaction_args =
        TransactionArgs::new(Some(script), None, AdviceMap::new(), Vec::<AccountInputs>::default());
    transaction_args.extend_output_note_recipients(vec![output_note.clone()]);

    Ok(transaction_args)
}
