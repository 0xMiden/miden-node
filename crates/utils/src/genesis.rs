use std::fmt;
use std::path::Path;

use anyhow::Context;
use miden_protocol::block::SignedBlock;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::{PublicKey, SigningKey};
use miden_protocol::utils::serde::{Deserializable, Serializable};

/// A predefined, insecure validator signing key for development purposes.
///
/// `miden-validator start` signs blocks with this key by default, and `miden-validator genesis`
/// commits the corresponding public key as the sole genesis validator by default, so a locally
/// bootstrapped chain works without any key configuration.
pub const INSECURE_VALIDATOR_SIGNING_KEY_HEX: &str =
    "0101010101010101010101010101010101010101010101010101010101010101";

/// Returns the public key of the predefined, insecure development validator signing key.
pub fn insecure_validator_public_key() -> PublicKey {
    let bytes = hex::decode(INSECURE_VALIDATOR_SIGNING_KEY_HEX)
        .expect("insecure development signing key hex is valid");
    SigningKey::read_from_bytes(&bytes)
        .expect("insecure development signing key bytes are a valid signing key")
        .public_key()
}

/// Returns the hex encoding of the insecure development validator public key, as committed by
/// `miden-validator genesis` when no validator keys are configured.
pub fn insecure_validator_public_key_hex() -> String {
    hex::encode(insecure_validator_public_key().to_bytes())
}

/// Official Miden networks with a hosted genesis block.
#[derive(clap::ValueEnum, Clone, Copy, Debug, Eq, PartialEq)]
pub enum OfficialNetwork {
    Devnet,
    Testnet,
}

impl OfficialNetwork {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Devnet => "devnet",
            Self::Testnet => "testnet",
        }
    }

    pub fn genesis_block_url(self) -> String {
        format!("https://genesis.{}.miden.io", self.as_str())
    }
}

impl fmt::Display for OfficialNetwork {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Reads a trusted genesis block from disk.
pub fn read_genesis_block(path: &Path) -> anyhow::Result<SignedBlock> {
    let bytes = fs_err::read(path).context("failed to read genesis block file")?;
    deserialize_genesis_block(&bytes)
}

/// Downloads a trusted genesis block for an official Miden network.
pub async fn fetch_genesis_block(network: OfficialNetwork) -> anyhow::Result<SignedBlock> {
    let url = network.genesis_block_url();
    let response = reqwest::get(url.as_str())
        .await
        .with_context(|| format!("failed to fetch genesis block from {url}"))?
        .error_for_status()
        .with_context(|| format!("failed to fetch genesis block from {url}"))?;
    let bytes = response
        .bytes()
        .await
        .with_context(|| format!("failed to read genesis block response from {url}"))?;

    deserialize_genesis_block(&bytes)
}

fn deserialize_genesis_block(bytes: &[u8]) -> anyhow::Result<SignedBlock> {
    SignedBlock::read_from_bytes(bytes).context("failed to deserialize genesis block")
}
