use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;
use miden_protocol::transaction::TransactionId;
use miden_protocol::utils::serde::Deserializable;
use miden_validator::{DataDirectory, GoldenOperatorKey, PrivateRecordShareRequest};
use rand_core_06::OsRng;

use super::PrivateRecordShareOptions;

/// Loads the required operator key and issues one local administrative share.
pub(super) async fn issue_from_options(options: PrivateRecordShareOptions) -> anyhow::Result<()> {
    let PrivateRecordShareOptions {
        data_directory,
        transaction_id,
        output,
        storage_key,
    } = options;
    let operator_key = storage_key.load()?;
    issue(data_directory, &transaction_id, output.as_deref(), &operator_key).await
}

/// Issues this validator's canonical share for one checked private record.
pub(super) async fn issue(
    data_directory: PathBuf,
    encoded_transaction_id: &str,
    output: Option<&Path>,
    operator_key: &GoldenOperatorKey,
) -> anyhow::Result<()> {
    let transaction_id = parse_transaction_id(encoded_transaction_id)?;
    let data_directory =
        DataDirectory::load(data_directory).context("failed to load validator data directory")?;
    let database = miden_validator::db::load(data_directory.database_path())
        .await
        .context("failed to load validator database")?;
    let record = database
        .read("load_private_record_for_share", move |tx| {
            miden_validator::db::load_private_record(tx, transaction_id)
        })
        .await
        .context("failed to load private record")?
        .with_context(|| format!("transaction {encoded_transaction_id} was not found"))?;

    let request = PrivateRecordShareRequest::for_record(&record);
    // Running this filesystem-restricted command is the demo's explicit release decision.
    let allow = |_: &PrivateRecordShareRequest, _: &miden_validator::StoredPrivateRecord| true;
    let share = operator_key
        .issue_private_record_share(&mut OsRng, &request, &record, &allow)
        .context("failed to issue private record share")?;

    write_share(output, &share)
}

/// Parses one canonical transaction identifier.
fn parse_transaction_id(encoded: &str) -> anyhow::Result<TransactionId> {
    let bytes = hex::decode(encoded).context("transaction id is not valid hex")?;
    TransactionId::read_from_bytes(&bytes).context("transaction id is not canonical")
}

/// Writes canonical share bytes without adding a text encoding or delimiter.
fn write_share(output: Option<&Path>, share: &[u8]) -> anyhow::Result<()> {
    if let Some(path) = output {
        fs_err::write(path, share)
            .with_context(|| format!("failed to write private record share to {}", path.display()))
    } else {
        let stdout = std::io::stdout();
        let mut stdout = stdout.lock();
        stdout.write_all(share).context("failed to write private record share")?;
        stdout.flush().context("failed to flush private record share")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transaction_id_requires_exact_canonical_bytes() {
        assert!(parse_transaction_id(&"01".repeat(32)).is_ok());
        assert!(parse_transaction_id("not hex").is_err());
        assert!(parse_transaction_id(&"01".repeat(31)).is_err());
        assert!(parse_transaction_id(&"01".repeat(33)).is_err());
    }

    #[test]
    fn file_output_preserves_share_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("share.bin");
        let share = [0, 1, 2, 3, 0xff];

        write_share(Some(&output), &share).unwrap();

        assert_eq!(fs_err::read(output).unwrap(), share);
    }
}
