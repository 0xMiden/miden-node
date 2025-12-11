use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use miden_node_utils::grpc::UrlExt;
use miden_node_validator::Validator;
use miden_objects::crypto::dsa::ecdsa_k256_keccak::SecretKey;
use miden_objects::utils::Deserializable;
use url::Url;

use crate::commands::{
    DEFAULT_TIMEOUT,
    ENV_ENABLE_OTEL,
    ENV_VALIDATOR_URL,
    duration_to_human_readable_string,
};

#[derive(clap::Subcommand)]
pub enum ValidatorCommand {
    /// Starts the validator component.
    Start {
        /// Url at which to serve the gRPC API.
        #[arg(env = ENV_VALIDATOR_URL)]
        url: Url,

        /// Enables the exporting of traces for OpenTelemetry.
        ///
        /// This can be further configured using environment variables as defined in the official
        /// OpenTelemetry documentation. See our operator manual for further details.
        #[arg(long = "enable-otel", default_value_t = true, env = ENV_ENABLE_OTEL, value_name = "BOOL")]
        enable_otel: bool,

        /// Maximum duration a gRPC request is allocated before being dropped by the server.
        #[arg(
            long = "grpc.timeout",
            default_value = &duration_to_human_readable_string(DEFAULT_TIMEOUT),
            value_parser = humantime::parse_duration,
            value_name = "DURATION"
        )]
        grpc_timeout: Duration,

        /// Filepath to the insecure validator secret key for signing blocks.
        ///
        /// Only used in development and testing environments.
        #[arg(long = "secret-key-filepath", value_name = "VALIDATOR_SECRET_KEY_FILEPATH")]
        secret_key_filepath: Option<PathBuf>,
    },
}

impl ValidatorCommand {
    pub async fn handle(self) -> anyhow::Result<()> {
        let Self::Start {
            url, grpc_timeout, secret_key_filepath, ..
        } = self;

        let Some(secret_key_filepath) = secret_key_filepath else {
            return Err(anyhow::anyhow!(
                "secret_key_filepath is required until other secret key backends are supported"
            ));
        };

        let address =
            url.to_socket().context("Failed to extract socket address from validator URL")?;

        // Read secret key file.
        let file_bytes = fs_err::read(&secret_key_filepath)?;
        let signer = SecretKey::read_from_bytes(&file_bytes)?;

        Validator { address, grpc_timeout, signer }
            .serve()
            .await
            .context("failed while serving validator component")
    }

    pub fn is_open_telemetry_enabled(&self) -> bool {
        let Self::Start { enable_otel, .. } = self;
        *enable_otel
    }
}
