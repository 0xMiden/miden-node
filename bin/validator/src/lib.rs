pub mod data_directory;
pub mod db;
mod private_record;
mod server;
mod signers;
mod storage_key;
mod tx_validation;

pub use data_directory::DataDirectory;
pub use private_record::{
    PRIVATE_RECORD_CIPHER_V1,
    PRIVATE_RECORD_SCHEMA_V1,
    PrivateRecordChainId,
    PrivateRecordCipher,
    PrivateRecordCombiner,
    PrivateRecordContext,
    PrivateRecordError,
    PrivateRecordId,
    PrivateRecordSealer,
    PrivateRecordSharePolicy,
    PrivateRecordShareRequest,
    PrivateRecordStorageFields,
    StoredPrivateRecord,
};
pub use server::ValidatorServer;
pub use signers::{
    KmsSigner,
    LocalX25519TransactionInputDecrypter,
    TransactionInputDecrypter,
    ValidatorSigner,
    decrypt_key_material,
};
pub use storage_key::{
    EncodedGoldenOperatorKey,
    GoldenOperatorKey,
    GoldenOperatorKeyError,
    StorageKeyEpoch,
};

// CONSTANTS
// =================================================================================================

/// The name of the validator component.
pub const COMPONENT: &str = "miden-validator";

/// The target to use for user-visible events.
pub const LOG_TARGET: &str = "user::miden-validator";
