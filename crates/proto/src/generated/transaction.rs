// This file is @generated by prost-build.
/// Submits proven transaction to the Miden network.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ProvenTransaction {
    /// Transaction encoded using \[winter_utils::Serializable\] implementation for
    /// \[miden_objects::transaction::proven_tx::ProvenTransaction\].
    #[prost(bytes = "vec", tag = "1")]
    pub transaction: ::prost::alloc::vec::Vec<u8>,
}
/// Represents a transaction ID.
#[derive(Clone, Copy, PartialEq, ::prost::Message)]
pub struct TransactionId {
    /// The transaction ID.
    #[prost(message, optional, tag = "1")]
    pub id: ::core::option::Option<super::primitives::Digest>,
}
/// Represents a transaction summary.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TransactionSummary {
    /// A unique 32-byte identifier of a transaction.
    #[prost(message, optional, tag = "1")]
    pub transaction_id: ::core::option::Option<TransactionId>,
    /// The block number in which the transaction was executed.
    #[prost(fixed32, tag = "2")]
    pub block_num: u32,
    /// The ID of the account affected by the transaction.
    #[prost(message, optional, tag = "3")]
    pub account_id: ::core::option::Option<super::account::AccountId>,
}
