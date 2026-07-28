---
title: "Public RPC"
sidebar_position: 1
---

# Public RPC

This page summarizes the public gRPC `rpc.Api` service.

As a reminder, you can inspect the exact schema on any deployed network using gRPC reflection:

```bash
grpcurl rpc.testnet.miden.io:443 describe rpc.Api
```

## Status and Limits

| Method      | Purpose                                                                                                   |
| ----------- | --------------------------------------------------------------------------------------------------------- |
| `Status`    | Returns the node RPC version, genesis commitment, store status, and block producer status when available. |
| `GetLimits` | Returns configured query parameter limits for methods that accept large repeated parameters.              |

## State Queries

| Method                   | Purpose                                                                          |
| ------------------------ | -------------------------------------------------------------------------------- |
| `GetAccount`             | Returns account witness data and optional details for public accounts.           |
| `GetBlockByNumber`       | Returns raw block data for a block number, optionally including the block proof. |
| `GetBlockHeaderByNumber` | Returns a block header and, optionally, MMR authentication data.                 |
| `GetNotesById`           | Returns committed notes matching the requested note IDs.                         |
| `GetNoteScriptByRoot`    | Returns a note script by script root when available.                             |

## Transaction Submission

| Method                        | Purpose                                                                                                 |
| ----------------------------- | ------------------------------------------------------------------------------------------------------- |
| `GetTransactionEncryptionKey` | Returns the current and next transaction encryption public keys, attested by a validator's signing key. |
| `SubmitProvenTx`              | Submits one proven transaction and returns the node's current block height.                             |
| `SubmitProvenTxBatch`         | Submits an atomic batch of proven transactions and returns the node's current block height.             |

The response carries the current key, its activation block, and an optional manually scheduled next key. Activations
must be epoch boundaries. One validator signature covers the complete schedule, including whether the next key is
absent, so an untrusted RPC cannot remove a scheduled key without invalidating the signature.

Clients should use `miden_node_proto::domain::transaction_encryption` to verify the response against a genesis
commitment, trusted chain tip, and validator signing keys. The signed attestation epoch must match the trusted tip's
epoch, which rejects responses replayed from an earlier epoch. The verifier also rejects premature current keys,
already-active next keys, and activations that are not epoch boundaries. Key IDs are opaque provider-owned bytes and
must be sent back with encrypted submissions so the provider can select the intended key.

Providers keep a schedule unchanged within an epoch. Preventing a validator from signing two different schedules in one
epoch requires this operational rule until the schedule has its own on-chain commitment.

This scheme does not hide transaction inputs from holders of the shared encryption secret and provides no forward
secrecy.

Write requests must identify the target network with the `genesis` parameter in the `Accept` header:

```text
application/vnd.miden; genesis=<genesis-commitment>
```

Clients may also include a compatible RPC version:

```text
application/vnd.miden; version=<semver>; genesis=<genesis-commitment>
```

See [Errors and Limits](./errors-and-limits.md#transaction-submission-errors) for the transaction submission detail
codes returned in gRPC status details.

## State Synchronization

| Method                   | Purpose                                                                                 |
| ------------------------ | --------------------------------------------------------------------------------------- |
| `SyncTransactions`       | Returns transaction records for specified accounts within a block range.                |
| `SyncNotes`              | Returns note metadata and inclusion proofs for matching note tags within a block range. |
| `SyncNullifiers`         | Returns nullifiers matching specified 16-bit prefixes within a block range.             |
| `SyncAccountVault`       | Returns public account vault updates within a block range.                              |
| `SyncAccountStorageMaps` | Returns public account storage map updates within a block range.                        |
| `SyncChainMmr`           | Returns MMR delta information needed to synchronize the chain MMR.                      |

Use `GetLimits` to discover the maximum request sizes accepted by the node before batching large sync requests.

## Streaming

| Method              | Purpose                                                                               |
| ------------------- | ------------------------------------------------------------------------------------- |
| `BlockSubscription` | Streams committed blocks from `block_from`, replaying history before live blocks.     |
| `ProofSubscription` | Streams block proofs from `block_from`, replaying existing proofs before live proofs. |

These streams are the primary mechanism full nodes use to replicate chain data from an upstream source. They are also
useful for indexers, explorers, and other services that need an append-only view of network progress.

## Network Note Debugging

| Method                 | Purpose                                                                                    |
| ---------------------- | ------------------------------------------------------------------------------------------ |
| `GetNetworkNoteStatus` | Returns the lifecycle status of a network note tracked by the network transaction builder. |
