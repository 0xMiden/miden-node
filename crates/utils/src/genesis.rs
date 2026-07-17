use std::fmt;
use std::path::Path;

use anyhow::Context;
use miden_protocol::block::{BlockHeader, BlockSignatures, SignedBlock};
use miden_protocol::utils::serde::Deserializable;

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

/// Reads a trusted, signed genesis block from disk.
pub fn read_signed_genesis_block(path: &Path) -> anyhow::Result<SignedBlock> {
    let bytes = fs_err::read(path).context("failed to read genesis block file")?;
    deserialize_signed_genesis_block(&bytes)
}

/// Downloads a trusted, signed genesis block for an official Miden network.
pub async fn fetch_signed_genesis_block(network: OfficialNetwork) -> anyhow::Result<SignedBlock> {
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

    deserialize_signed_genesis_block(&bytes)
}

fn deserialize_signed_genesis_block(bytes: &[u8]) -> anyhow::Result<SignedBlock> {
    SignedBlock::read_from_bytes(bytes).context("failed to deserialize genesis block")
}

/// Verifies the signatures of a genesis block against its own header.
///
/// The genesis block has no parent, so it acts as the chain's trust root: it carries exactly one
/// signature, produced by the bootstrapping validator, which must verify against a key in the
/// validator set committed to by its own header. The full committed set is only required to sign
/// from the next block onwards, so bootstrapping needs a single validator key.
pub fn verify_genesis_signatures(
    header: &BlockHeader,
    signatures: &BlockSignatures,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        signatures.len() == 1,
        "genesis block must carry exactly one bootstrapper signature, got {}",
        signatures.len(),
    );
    let signature = &signatures.as_signatures()[0];
    let commitment = header.commitment();
    anyhow::ensure!(
        header
            .validator_keys()
            .as_keys()
            .iter()
            .any(|key| signature.verify(commitment, key)),
        "genesis block signature does not verify against any key in the committed validator set",
    );
    Ok(())
}
