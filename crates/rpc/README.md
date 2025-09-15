# Miden node RPC

Contains the code defining the [Miden node's RPC component](/README.md#architecture). This component serves the
user-facing [gRPC](https://grpc.io) methods used to submit transactions and sync with the state of the network.

This is the **only** set of node RPC methods intended to be publicly available.

For more information on the installation and operation of this component, please see the [node's readme](/README.md).

## API overview

The full gRPC method definitions can be found in the [proto](../proto/README.md) crate.

<!--toc:start-->

- [CheckNullifiers](#checknullifiers)
- [SyncNullifiers](#syncnullifiers)
- [GetAccountDetails](#getaccountdetails)
- [GetAccountProofs](#getaccountproofs)
- [GetBlockByNumber](#getblockbynumber)
- [GetBlockHeaderByNumber](#getblockheaderbynumber)
- [GetNotesById](#getnotesbyid)
- [GetNoteScriptByRoot](#getnotescriptbyroot)
- [SubmitProvenTransaction](#submitproventransaction)
- [SyncAccountVault](#SyncAccountVault)
- [SyncNotes](#syncnotes)
- [SyncState](#syncstate)
- [SyncStorageMaps](#syncstoragemaps)
- [SyncTransactions](#synctransactions)

<!--toc:end-->

---

### CheckNullifiers

Returns a nullifier proof for each of the requested nullifiers.

---

### GetAccountDetails

Returns the latest state of an account with the specified ID.

---

### GetAccountProofs

Returns the latest state proofs of the specified accounts.

---

### GetBlockByNumber

Returns raw block data for the specified block number.

---

### GetBlockHeaderByNumber

Retrieves block header by given block number. Optionally, it also returns the MMR path and current chain length to
authenticate the block's inclusion.

---

### GetNotesById

Returns a list of notes matching the provided note IDs.

---

### GetNoteScriptByRoot

Returns the script for a note by its root.

---

### SubmitProvenTransaction

Submits proven transaction to the Miden network.

---

### SyncNullifiers

Returns nullifier synchronization data for a set of prefixes within a given block range. This method allows
clients to efficiently track nullifier creation by retrieving only the nullifiers produced between two blocks.

Caller specifies the `prefix_len` (currently only 16), the list of prefix values (`nullifiers`), and the block
range (`from_start_block`, optional `to_end_block`). The response includes all matching nullifiers created within that
range, the last block included in the response (`block_num`), and the current chain tip (`chain_tip`).

If the response is chunked due to exceeding the maximum returned entries, continue by issuing another request with
consecutive block number to retrieve subsequent updates.

---

### SyncAccountVault

Returns information that allows clients to sync asset values for specific public accounts within a block range.

For any `block_range`, the latest known set of assets is returned for the requested account ID.
The data can be split and a cutoff block may be selected if there are too many assets to sync. The response contains
the chain tip so that the caller knows when it has been reached.

---

### SyncNotes

Returns info which can be used by the client to sync up to the tip of chain for the notes they are interested in.

Client specifies the `note_tags` they are interested in, and the block height from which to search for new for matching
notes for. The request will then return the next block containing any note matching the provided tags.

The response includes each note's metadata and inclusion proof.

A basic note sync can be implemented by repeatedly requesting the previous response's block until reaching the tip of
the chain.

---

### SyncState

Returns info which can be used by the client to sync up to the latest state of the chain for the objects (accounts and
notes) the client is interested in.

This request returns the next block containing requested data. It also returns `chain_tip` which is the latest block
number in the chain. Client is expected to repeat these requests in a loop until
`response.block_header.block_num == response.chain_tip`, at which point the client is fully synchronized with the chain.

Each request also returns info about new notes, accounts, etc. created. It also returns Chain MMR delta that can be
used to update the state of Chain MMR. This includes both chain MMR peaks and chain MMR nodes.

For preserving some degree of privacy, note tags contain only high part of hashes. Thus, returned data contains excessive
notes, client can make additional filtering of that data on its side.

---

### SyncStorageMaps

Returns storage map synchronization data for a specified public account within a given block range. This method allows clients to efficiently sync the storage map state of an account by retrieving only the changes that occurred between two blocks.

Caller specifies the `account_id` of the public account and the block range `block_range` for which to retrieve storage updates. The response includes all storage map key-value updates that occurred within that range, along with the last block included in the sync and the current chain tip.

This endpoint enables clients to maintain an updated view of account storage.

---

### SyncTransactions

Returns transaction records for specific accounts within a block range.

---

## License

This project is [MIT licensed](../../LICENSE).
