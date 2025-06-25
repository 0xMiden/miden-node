use std::{convert::Infallible, str::FromStr};

use miden_objects::{NetworkIdError, account::NetworkId};
use serde::{Deserialize, Serialize};

// NETWORK
// ================================================================================================

/// Represents the network where the faucet is running. It is used to show the correct bech32
/// addresses and explorer URL in the UI.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum Network {
    Testnet,
    Devnet,
    Localhost,
    Custom(String),
}

impl FromStr for Network {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Infallible> {
        match s.to_lowercase().as_str() {
            "devnet" => Ok(Network::Devnet),
            "localhost" => Ok(Network::Localhost),
            "testnet" => Ok(Network::Testnet),
            custom => Ok(Network::Custom(custom.to_string())),
        }
    }
}

impl Network {
    /// Converts the network configuration to a network ID.
    pub fn to_network_id(&self) -> Result<NetworkId, NetworkIdError> {
        Ok(match self {
            Network::Testnet => NetworkId::Testnet,
            Network::Devnet => NetworkId::Devnet,
            Network::Localhost => NetworkId::new("mlcl")?,
            Network::Custom(s) => NetworkId::new(s)?,
        })
    }
}

/// A type wrapper for the explorer URL.
#[derive(Serialize)]
pub struct ExplorerUrl(&'static str);

impl ExplorerUrl {
    /// Returns the explorer URL for the given network ID.
    /// Currently only testnet explorer is available.
    pub fn from_network_id(network_id: NetworkId) -> Option<Self> {
        match network_id {
            NetworkId::Testnet => Some(Self("https://testnet.midenscan.com")),
            _ => None,
        }
    }
}
