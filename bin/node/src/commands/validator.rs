use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use miden_node_utils::grpc::UrlExt;
use miden_node_validator::Validator;
use url::Url;

use crate::commands::{
    DEFAULT_TIMEOUT,
    ENV_DATA_DIRECTORY,
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

        /// Directory in which to store the database and raw block data.
        #[arg(long, env = ENV_DATA_DIRECTORY, value_name = "DIR")]
        data_directory: PathBuf,
    },
}

impl ValidatorCommand {
    pub async fn handle(self) -> anyhow::Result<()> {
        let Self::Start { url, grpc_timeout, data_directory, .. } = self;

        let address =
            url.to_socket().context("Failed to extract socket address from validator URL")?;

        Validator { address, grpc_timeout, data_directory }
            .serve()
            .await
            .context("failed while serving validator component")
    }

    pub fn is_open_telemetry_enabled(&self) -> bool {
        let Self::Start { enable_otel, .. } = self;
        *enable_otel
    }
}
