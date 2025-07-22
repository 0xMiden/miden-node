use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::{Display, Formatter},
    net::SocketAddr,
    num::NonZeroU32,
};

use itertools::Itertools;
use miden_node_proto::{
    AccountState,
    domain::batch::BatchInputs,
    errors::{ConversionError, MissingFieldHelper},
    generated::{self as proto, block_producer_store::block_producer_client as store_client},
};
use miden_node_utils::{formatting::format_opt, tracing::grpc::OtelInterceptor};
use miden_objects::{
    Word,
    account::AccountId,
    block::{BlockHeader, BlockInputs, BlockNumber, ProvenBlock},
    note::{NoteId, Nullifier},
    transaction::ProvenTransaction,
    utils::Serializable,
};
use tonic::{service::interceptor::InterceptedService, transport::Channel};
use tracing::{debug, info, instrument};

use crate::{COMPONENT, errors::StoreError};

// TRANSACTION INPUTS
// ================================================================================================

/// Information needed from the store to verify a transaction.
#[derive(Debug)]
pub struct TransactionInputs {
    /// Account ID
    pub account_id: AccountId,
    /// The account commitment in the store corresponding to tx's account ID
    pub account_commitment: Option<Word>,
    /// Maps each consumed notes' nullifier to block number, where the note is consumed.
    ///
    /// We use `NonZeroU32` as the wire format uses 0 to encode none.
    pub nullifiers: BTreeMap<Nullifier, Option<NonZeroU32>>,
    /// Unauthenticated notes which are present in the store.
    ///
    /// These are notes which were committed _after_ the transaction was created.
    pub found_unauthenticated_notes: BTreeSet<NoteId>,
    /// The current block height.
    pub current_block_height: BlockNumber,
}

impl Display for TransactionInputs {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let nullifiers = self
            .nullifiers
            .iter()
            .map(|(k, v)| format!("{k}: {}", format_opt(v.as_ref())))
            .join(", ");

        let nullifiers = if nullifiers.is_empty() {
            "None".to_owned()
        } else {
            format!("{{ {nullifiers} }}")
        };

        f.write_fmt(format_args!(
            "{{ account_id: {}, account_commitment: {}, nullifiers: {} }}",
            self.account_id,
            format_opt(self.account_commitment.as_ref()),
            nullifiers
        ))
    }
}

impl TryFrom<proto::block_producer_store::TransactionInputs> for TransactionInputs {
    type Error = ConversionError;

    fn try_from(
        response: proto::block_producer_store::TransactionInputs,
    ) -> Result<Self, Self::Error> {
        let AccountState { account_id, account_commitment } = response
            .account_state
            .ok_or(proto::block_producer_store::TransactionInputs::missing_field(stringify!(
                account_state
            )))?
            .try_into()?;

        let mut nullifiers = BTreeMap::new();
        for nullifier_record in response.nullifiers {
            let nullifier = nullifier_record
                .nullifier
                .ok_or(
                    proto::block_producer_store::transaction_inputs::NullifierTransactionInputRecord::missing_field(
                        stringify!(nullifier),
                    ),
                )?
                .try_into()?;

            // Note that this intentionally maps 0 to None as this is the definition used in
            // protobuf.
            nullifiers.insert(nullifier, NonZeroU32::new(nullifier_record.block_num));
        }

        let found_unauthenticated_notes = response
            .found_unauthenticated_notes
            .into_iter()
            .map(|digest| Ok(Word::try_from(digest)?.into()))
            .collect::<Result<_, ConversionError>>()?;

        let current_block_height = response.block_height.into();

        Ok(Self {
            account_id,
            account_commitment,
            nullifiers,
            found_unauthenticated_notes,
            current_block_height,
        })
    }
}

// STORE CLIENT
// ================================================================================================

type InnerClient = store_client::BlockProducerClient<InterceptedService<Channel, OtelInterceptor>>;

/// Interface to the store's block-producer gRPC API.
///
/// Essentially just a thin wrapper around the generated gRPC client which improves type safety.
#[derive(Clone, Debug)]
pub struct StoreClient {
    inner: InnerClient,
}

impl StoreClient {
    /// Creates a new store client with a lazy connection.
    pub fn new(store_address: SocketAddr) -> Self {
        let store_url = format!("http://{store_address}");
        // SAFETY: The store_url is always valid as it is created from a `SocketAddr`.
        let channel = tonic::transport::Endpoint::try_from(store_url).unwrap().connect_lazy();
        let store = store_client::BlockProducerClient::with_interceptor(channel, OtelInterceptor);
        info!(target: COMPONENT, store_endpoint = %store_address, "Store client initialized");

        Self { inner: store }
    }

    /// Returns the latest block's header from the store.
    #[instrument(target = COMPONENT, name = "store.client.latest_header", skip_all, err)]
    pub async fn latest_header(&self) -> Result<BlockHeader, StoreError> {
        let response = self
            .inner
            .clone()
            .get_block_header_by_number(tonic::Request::new(
                proto::shared::BlockHeaderByNumberRequest::default(),
            ))
            .await?
            .into_inner()
            .block_header
            .ok_or(miden_node_proto::generated::blockchain::BlockHeader::missing_field(
                "block_header",
            ))?;

        BlockHeader::try_from(response).map_err(Into::into)
    }

    #[instrument(target = COMPONENT, name = "store.client.get_tx_inputs", skip_all, err)]
    pub async fn get_tx_inputs(
        &self,
        proven_tx: &ProvenTransaction,
    ) -> Result<TransactionInputs, StoreError> {
        let message = proto::block_producer_store::GetTransactionInputsRequest {
            account_id: Some(proven_tx.account_id().into()),
            nullifiers: proven_tx.nullifiers().map(Into::into).collect(),
            unauthenticated_notes: proven_tx
                .unauthenticated_notes()
                .map(|note| note.id().into())
                .collect(),
        };

        info!(target: COMPONENT, tx_id = %proven_tx.id().to_hex());
        debug!(target: COMPONENT, ?message);

        let request = tonic::Request::new(message);
        let response = self.inner.clone().get_transaction_inputs(request).await?.into_inner();

        debug!(target: COMPONENT, ?response);

        let tx_inputs: TransactionInputs = response.try_into()?;

        if tx_inputs.account_id != proven_tx.account_id() {
            return Err(StoreError::MalformedResponse(format!(
                "incorrect account id returned from store. Got: {}, expected: {}",
                tx_inputs.account_id,
                proven_tx.account_id()
            )));
        }

        debug!(target: COMPONENT, %tx_inputs);

        Ok(tx_inputs)
    }

    #[instrument(target = COMPONENT, name = "store.client.get_block_inputs", skip_all, err)]
    pub async fn get_block_inputs(
        &self,
        updated_accounts: impl Iterator<Item = AccountId> + Send,
        created_nullifiers: impl Iterator<Item = Nullifier> + Send,
        unauthenticated_notes: impl Iterator<Item = NoteId> + Send,
        reference_blocks: impl Iterator<Item = BlockNumber> + Send,
    ) -> Result<BlockInputs, StoreError> {
        let request = tonic::Request::new(proto::block_producer_store::BlockInputsRequest {
            account_ids: updated_accounts.map(Into::into).collect(),
            nullifiers: created_nullifiers.map(proto::primitives::Digest::from).collect(),
            unauthenticated_notes: unauthenticated_notes
                .map(proto::primitives::Digest::from)
                .collect(),
            reference_blocks: reference_blocks.map(|block_num| block_num.as_u32()).collect(),
        });

        let store_response = self.inner.clone().get_block_inputs(request).await?.into_inner();

        store_response.try_into().map_err(Into::into)
    }

    #[instrument(target = COMPONENT, name = "store.client.get_batch_inputs", skip_all, err)]
    pub async fn get_batch_inputs(
        &self,
        block_references: impl Iterator<Item = (BlockNumber, Word)> + Send,
        notes: impl Iterator<Item = NoteId> + Send,
    ) -> Result<BatchInputs, StoreError> {
        let request = tonic::Request::new(proto::block_producer_store::GetBatchInputsRequest {
            reference_blocks: block_references.map(|(block_num, _)| block_num.as_u32()).collect(),
            note_ids: notes.map(proto::primitives::Digest::from).collect(),
        });

        let store_response = self.inner.clone().get_batch_inputs(request).await?.into_inner();

        store_response.try_into().map_err(Into::into)
    }

    #[instrument(target = COMPONENT, name = "store.client.apply_block", skip_all, err)]
    pub async fn apply_block(&self, block: &ProvenBlock) -> Result<(), StoreError> {
        let request = tonic::Request::new(proto::blockchain::Block { block: block.to_bytes() });

        self.inner.clone().apply_block(request).await.map(|_| ()).map_err(Into::into)
    }
}
