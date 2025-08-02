# Changelog

## v0.11.0 (TBD)

### Changes

- [BREAKING] Updated MSRV to 1.88.
- [BREAKING] Refactor protobuf messages ([#1045](https://github.com/0xMiden/miden-node/pull/#1045)).
- [BREAKING] Consolidate to a single async interface and drop `#[maybe_async]` usage ([#1666](https://github.com/0xMiden/miden-node/pull/#1666))

### Enhancements

- Added environment variable support for batch and block size CLI arguments ([#1081](https://github.com/0xMiden/miden-node/pull/1081)).
- A transaction's account delta is now checked against its commitments in `SubmitProvenTransaction` endpoint ([#1093](https://github.com/0xMiden/miden-node/pull/1093)).
- Added check for Account Id prefix uniqueness when transactions to create accounts are submitted to the mempool ([#1094](https://github.com/0xMiden/miden-node/pull/1094)).
- RPC accept header now supports specifying the genesis commitment in addition to the RPC version. This lets clients ensure they are on the right network ([#1084](https://github.com/0xMiden/miden-node/pull/1084)).

### Changes

- [BREAKING] Updated MSRV to 1.88.
- [BREAKING] RPC accept header format changed from `application/miden.vnd+grpc.<version>` to `application/vnd.miden; version=<version>` ([#1084](https://github.com/0xMiden/miden-node/pull/1084)).

### Fixes

- [BREAKING] Integrated proxy status endpoint into main proxy service, removing separate status port.
- RPC requests with wildcard (`*/*`) media-type are not longer rejected ([#1084](https://github.com/0xMiden/miden-node/pull/1084)).
- Stress-test CLI account now properly sets the storage mode and increment nonce in transactions ([#1113](https://github.com/0xMiden/miden-node/pull/1113)).

## v0.10.1 (2025-07-14)

### Fixes

- Network accounts are no longer disabled after one transaction ([#1086](https://github.com/0xMiden/miden-node/pull/1086)).

## v0.10.0 (2025-07-10)

### Enhancements

- Added `miden-proving-service` and `miden-proving-service-client` crates (#926).
- Added support for gRPC server side reflection to all components (#949).
- Added support for TLS to `miden-proving-service-client` (#968).
- Added support for TLS to faucet's connection to node RPC (#976).
- Replaced integer-based duration args with human-readable duration strings (#998 & #1014).
- [BREAKING] Refactor the `miden-proving-service` proxy status service to use gRPC instead of HTTP (#953).
- Genesis state is now configurable during bootstrapping (#1000)
- Added configurable network id for the faucet (#1016).
- Network transaction builder now tracks inflight txs instead of only committed ones (#1051).
- Add open-telemetry trace layers to `miden-remote-prover` and `miden-remote-prover-proxy` (#1061).
- Add open-telemetry stats for the mempool (#1073).
- Add open-telemetry stats for the network transaction builder state (#1073).

### Changes

- Faucet `PoW` difficulty is now configurable (#924).
- Separated the store API into three separate services (#932).
- Added a faucet Dockerfile (#933).
- Exposed `miden-proving-service` as a library (#956).
- [BREAKING] Update `RemoteProverError::ConnectionFailed` variant to contain `Error` instead of `String` (#968).
- [BREAKING] Replace faucet TOML configuration file with flags and env vars (#976).
- [BREAKING] Replace faucet Init command with CreateApiKeys command (#976).
- [BREAKING] Consolidate default account filepath for bundled bootstrap and faucet start commands to `account.mac` (#976).
- [BREAKING] Remove default value account filepath for faucet commands and rename --output-path to --output (#976).
- [BREAKING] Enforce `PoW` on all faucet API key-authenticated requests (#974).
- Compressed faucet background image (#985).
- Remove faucet rate limiter by IP and API Key, this has been superseded by PoW (#1011).
- Transaction limit per batch is now configurable (default 8) (#1015).
- Batch limit per block is now configurable (default 8) (#1015).
- Faucet challenge expiration time is now configurable (#1017).
- Removed system monitor from node binary (#1019).
- [BREAKING] Renamed `open_telemetry` to `enable_otel` in all node's commands (#1019).
- [BREAKING] Rename `miden-proving-service` to `miden-remote-prover` (#1004).
- [BREAKING] Rename `miden-proving-service-client` to `miden-remote-prover-client` (#1004).
- [BREAKING] Rename `RemoteProverError` to `RemoteProverClientError` (#1004).
- [BREAKING] Rename `ProvingServiceError` to `RemoteProverError` (#1004).
- [BREAKING] Renamed `Note` to `CommittedNote`, and `NetworkNote` to `Note` in the proto messages (#1022).
- [BREAKING] Limits of store queries per query parameter enforced (#1028).
- Support gRPC server reflection `v1alpha` (#1036).
- Migrate from `rusqlite` to `diesel` as a database abstraction (#921)

### Fixes

- Faucet considers decimals when minting token amounts (#962).

## v0.9.2 (2025-06-12)

- Refresh Cargo.lock file.

## v0.9.1 (2025-06-10)

- Refresh Cargo.lock file (#944).

## v0.9.0 (2025-05-30)

### Enhancements

- Enabled running RPC component in `read-only` mode (#802).
- Added gRPC `/status` endpoint on all components (#817).
- Block producer now emits network note information (#833).
- Introduced Network Transaction Builder (#840).
- Added way of executing and proving network transactions (#841).
- [BREAKING] Add HTTP ACCEPT header layer to RPC server to enforce semver requirements against client connections (#844).

### Changes

- [BREAKING] Simplified node bootstrapping (#776).
  - Database is now created during bootstrap process instead of on first startup.
  - Data directory is no longer created but is instead expected to exist.
  - The genesis block can no longer be configured which also removes the `store dump-genesis` command.
- [BREAKING] Use `AccountTree` and update account witness proto definitions (#783).
- [BREAKING] Update name of `ChainMmr` to `PartialBlockchain` (#807).
- Added `--enable-otel` and `MIDEN_FAUCET_ENABLE_OTEL` flag to faucet (#834).
- Faucet now supports the usage of a remote transaction prover (#830).
- Added a required Proof-of-Work in the faucet to request tokens (#831).
- Added an optional API key request parameter to skip PoW in faucet (#839).
- Proof-of-Work difficulty is now adjusted based on the number of concurrent requests (#865).
- Added options for configuring NTB in `bundled` command (#884).
- [BREAKING] Updated MSRV to 1.87.

### Fixes

- Prevents duplicated note IDs (#842).

## v0.8.2 (2025-05-04)

### Enhancements

- gRPC error messages now include more context (#819).
- Faucet now detects and recovers from state desync (#819).
- Faucet implementation is now more robust (#819).
- Faucet now supports TLS connection to the node RPC (#819).

### Fixes

- Faucet times out during high load (#819).

## v0.8.0 (2025-03-26)

### Enhancements

- Implemented database optimization routine (#721).

### Fixes

- Faucet webpage is missing `background.png` and `favicon.ico` (#672).

### Enhancements

- Add an optional open-telemetry trace exporter (#659, #690).
- Support tracing across gRPC boundaries using remote tracing context (#669).
- Instrument the block-producer's block building process (#676).
- Use `LocalBlockProver` for block building (#709).
- Initial developer and operator guides covering monitoring (#699).
- Instrument the block-producer's batch building process (#738).
- Optimized database by adding missing indexes (#728).
- Added support for `Content-type` header in `get_tokens` endpoint of the faucet (#754).
- Block frequency is now configurable (#750).

### Changes

- [BREAKING] `Endpoint` configuration simplified to a single string (#654).
- Added stress test binary with seed-store command (#657).
- [BREAKING] `CheckNullifiersByPrefix` now takes a starting block number (#707).
- [BREAKING] Removed nullifiers from `SyncState` endpoint (#708).
- [BREAKING] Update `GetBlockInputs` RPC (#709).
- [BREAKING] Added `batch_prover_url` to block producer configuration (#701).
- [BREAKING] Added `block_prover_url` to block producer configuration (#719).
- [BREAKING] Removed `miden-rpc-proto` and introduced `miden-node-proto-build` (#723).
- [BREAKING] Updated to Rust Edition 2024 (#727).
- [BREAKING] MSRV bumped to 1.85 (#727).
- [BREAKING] Replaced `toml` configuration with CLI (#732).
- [BREAKING] Renamed multiple `xxx_hash` to `xxx_commitment` in RPC API (#757).

### Enhancements

- Prove transaction batches using Rust batch prover reference implementation (#659).

## v0.7.2 (2025-01-29)

### Fixes

- Faucet webpage rejects valid account IDs (#655).

## v0.7.1 (2025-01-28)

### Fixes

- Faucet webpage fails to load styling (index.css) and script (index.js) (#647).

### Changes

- [BREAKING] Default faucet endpoint is now public instead of localhost (#647).

## v0.7.0 (2025-01-23)

### Enhancements

- Support Https in endpoint configuration (#556).
- Upgrade `block-producer` from FIFO queue to mempool dependency graph (#562).
- Support transaction expiration (#582).
- Improved RPC endpoints doc comments (#620).

### Changes

- Standardized protobuf type aliases (#609).
- [BREAKING] Added support for new two `Felt` account ID (#591).
- [BREAKING] Inverted `TransactionInputs.missing_unauthenticated_notes` to `found_missing_notes` (#509).
- [BREAKING] Remove store's `ListXXX` endpoints which were intended for test purposes (#608).
- [BREAKING] Added support for storage maps on `GetAccountProofs` endpoint (#598).
- [BREAKING] Removed the `testing` feature (#619).
- [BREAKING] Renamed modules to singular (#636).

## v0.6.0 (2024-11-05)

### Enhancements

- Added `GetAccountProofs` endpoint (#506).

### Changes

- [BREAKING] Added `kernel_root` to block header's protobuf message definitions (#496).
- [BREAKING] Renamed `off-chain` and `on-chain` to `private` and `public` respectively for the account storage modes (#489).
- Optimized state synchronizations by removing unnecessary fetching and parsing of note details (#462).
- [BREAKING] Changed `GetAccountDetailsResponse` field to `details` (#481).
- Improve `--version` by adding build metadata (#495).
- [BREAKING] Introduced additional limits for note/account number (#503).
- [BREAKING] Removed support for basic wallets in genesis creation (#510).
- Migrated faucet from actix-web to axum (#511).
- Changed the `BlockWitness` to pass the inputs to the VM using only advice provider (#516).
- [BREAKING] Improved store API errors (return "not found" instead of "internal error" status if requested account(s) not found) (#518).
- Added `AccountCode` as part of `GetAccountProofs` endpoint response (#521).
- [BREAKING] Migrated to v0.11 version of Miden VM (#528).
- Reduce cloning in the store's `apply_block` (#532).
- [BREAKING] Changed faucet storage type in the genesis to public. Using faucet from the genesis for faucet web app. Added support for faucet restarting without blockchain restarting (#517).
- [BREAKING] Improved `ApplyBlockError` in the store (#535).
- [BREAKING] Updated minimum Rust version to 1.82.

## 0.5.1 (2024-09-12)

### Enhancements

- Node component server startup is now coherent instead of requiring an arbitrary sleep amount (#488).

## 0.5.0 (2024-08-27)

### Enhancements

- [BREAKING] Configuration files with unknown properties are now rejected (#401).
- [BREAKING] Removed redundant node configuration properties (#401).
- Support multiple inflight transactions on the same account (#407).
- Now accounts for genesis are optional. Accounts directory will be overwritten, if `--force` flag is set (#420).
- Added `GetAccountStateDelta` endpoint (#418).
- Added `CheckNullifiersByPrefix` endpoint (#419).
- Added `GetNoteAuthenticationInfo` endpoint (#421).
- Added `SyncNotes` endpoint (#424).
- Added `execution_hint` field to the `Notes` table (#441).

### Changes

- Improve type safety of the transaction inputs nullifier mapping (#406).
- Embed the faucet's static website resources (#411).
- CI check for proto file consistency (#412).
- Added warning on CI for `CHANGELOG.md` (#413).
- Implemented caching of SQL statements (#427).
- Updates to `miden-vm` dependency to v0.10 and `winterfell` dependency to v0.9 (#457).
- [BREAKING] Updated minimum Rust version to 1.80 (#457).

### Fixes

- `miden-node-proto`'s build script always triggers (#412).

## 0.4.0 (2024-07-04)

### Features

- Changed sync endpoint to return a list of committed transactions (#377).
- Added `aux` column to notes table (#384).
- Changed state sync endpoint to return a list of `TransactionSummary` objects instead of just transaction IDs (#386).
- Added support for unauthenticated transaction notes (#390).

### Enhancements

- Standardized CI and Makefile across Miden repositories (#367)
- Removed client dependency from faucet (#368).
- Fixed faucet note script so that it uses the `aux` input (#387).
- Added crate to distribute node RPC protobuf files (#391).
- Add `init` command for node and faucet (#392).

## 0.3.0 (2024-05-15)

- Added option to mint public notes in the faucet (#339).
- Renamed `note_hash` into `note_id` in the database (#336)
- Changed `version` and `timestamp` fields in `Block` message to `u32` (#337).
- [BREAKING] Implemented `NoteMetadata` protobuf message (#338).
- Added `GetBlockByNumber` endpoint (#340).
- Added block authentication data to the `GetBlockHeaderByNumber` RPC (#345).
- Enabled support for HTTP/1.1 requests for the RPC component (#352).

## 0.2.1 (2024-04-27)

- Combined node components into a single binary (#323).

## 0.2.0 (2024-04-11)

- Implemented Docker-based node deployment (#257).
- Improved build process (#267, #272, #278).
- Implemented Nullifier tree wrapper (#275).
- [BREAKING] Added support for public accounts (#287, #293, #294).
- [BREAKING] Added support for public notes (#300, #310).
- Added `GetNotesById` endpoint (#298).
- Implemented amd64 debian packager (#312).

## 0.1.0 (2024-03-11)

- Initial release.
