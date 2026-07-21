pub mod data_directory;
pub mod db;
mod server;
mod signers;
mod tx_validation;

pub use data_directory::DataDirectory;
pub use server::ValidatorServer;
pub use signers::{
    KmsSigner,
    LocalX25519TransactionInputDecrypter,
    TransactionEncryptionKeyInfo,
    TransactionInputDecrypter,
    ValidatorSigner,
    attestation_commitment,
    decrypt_key_material,
};

// CONSTANTS
// =================================================================================================

/// The name of the validator component.
pub const COMPONENT: &str = "miden-validator";

/// The target to use for user-visible events.
pub const LOG_TARGET: &str = "user::miden-validator";
