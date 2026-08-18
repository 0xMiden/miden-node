use std::num::NonZeroUsize;
use std::time::Duration;

use miden_node_store::GenesisState;
use miden_node_store::state::State;
use miden_node_utils::fee::test_fee_params;
use miden_protocol::block::{BlockNumber, ValidatorKeys};
use miden_protocol::testing::random_secret_key::random_secret_key;
use url::Url;

use crate::{
    DEFAULT_BATCH_WORKERS, DEFAULT_MAX_BATCHES_PER_BLOCK, DEFAULT_MAX_CONCURRENT_PROOFS,
    DEFAULT_MAX_TXS_PER_BATCH, DEFAULT_VALIDATOR_TIMEOUT, Sequencer,
};

#[tokio::test]
async fn block_producer_starts_with_store_state() {
    let data_directory = tempfile::tempdir().expect("tempdir should be created");
    bootstrap_store(data_directory.path());
    let (state, block_writer, proof_writer) = State::for_tests(data_directory.path()).await;

    let block_producer = Sequencer {
        state,
        block_writer,
        proof_writer,
        validator_urls: vec![Url::parse("http://127.0.0.1:0").unwrap()],
        validator_timeout: DEFAULT_VALIDATOR_TIMEOUT,
        batch_prover_url: None,
        block_prover_url: None,
        batch_interval: Duration::from_hours(1),
        block_interval: Duration::from_hours(1),
        max_txs_per_batch: DEFAULT_MAX_TXS_PER_BATCH,
        max_batches_per_block: DEFAULT_MAX_BATCHES_PER_BLOCK,
        max_concurrent_proofs: DEFAULT_MAX_CONCURRENT_PROOFS,
        mempool_tx_capacity: NonZeroUsize::new(100).unwrap(),
        batch_workers: DEFAULT_BATCH_WORKERS,
    }
    .spawn(miden_node_utils::shutdown::CancellationToken::new())
    .unwrap();

    let status = block_producer.api().status().await;
    assert_eq!(status.status, "connected");
    assert_eq!(status.chain_tip, BlockNumber::GENESIS);
}

fn bootstrap_store(path: &std::path::Path) {
    let signer = random_secret_key();
    let genesis_state = GenesisState::new(
        vec![],
        test_fee_params(),
        1,
        1,
        ValidatorKeys::new(vec![signer.public_key()]).unwrap(),
    );
    let genesis_block = genesis_state
        .into_block()
        .expect("genesis block should be created");

    State::bootstrap(genesis_block, path).expect("store should bootstrap");
}
