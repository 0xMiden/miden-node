# Miden node store

Contains the code defining the [Miden node's store component](/README.md#architecture). This component stores the
network's state and acts as the networks source of truth. It serves a [gRPC](https://grpc.io) API which allow the other
node components to interact with the store. This API is **internal** only and is considered trusted i.e. the node
operator must take care that the store's API endpoint is **only** exposed to the other node components.

For more information on the installation and operation of this component, please see the [node's readme](/README.md).

## API overview

The full gRPC API can be found [here](../../proto/proto/store.proto).

<!--toc:start-->
- [ApplyBlock](#applyblock)
- [CheckNullifiers](#checknullifiers)
- [GetAccountDetails](#getaccountdetails)
- [GetAccountProofs](#getaccountproofs)
- [GetBlockByNumber](#getblockbynumber)
- [GetBlockHeaderByNumber](#getblockheaderbynumber)
- [GetBlockInputs](#getblockinputs)
- [GetNoteAuthenticationInfo](#getnoteauthenticationinfo)
- [GetNotesById](#getnotesbyid)
- [GetTransactionInputs](#gettransactioninputs)
- [GetNoteScriptByRoot](#getnotescriptbyroot)
- [SyncNullifiers](#syncnullifiers)
- [SyncAccountVault](#syncaccountvault)
- [SyncNotes](#syncnotes)
- [SyncState](#syncstate)
- [SyncStorageMaps](#syncstoragemaps)
<!--toc:end-->

---

### ApplyBlock

Applies changes of a new block to the DB and in-memory data structures. Raw block data is also stored as a flat file.

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

### GetBlockInputs

Used by the `block-producer` to query state required to prove the next block.

---

### GetNoteAuthenticationInfo

Returns a list of Note inclusion proofs for the specified Note IDs.

This is used by the `block-producer` as part of the batch proving process.

---

### GetNotesById

Returns a list of notes matching the provided note IDs.

---

### GetTransactionInputs

Used by the `block-producer` to query state required to verify a submitted transaction.

---

### GetNoteScriptByRoot

Returns the script for a note by its root.

---

### SyncNullifiers

Returns nullifier synchronization data for a set of prefixes within a given block range. This method allows clients to efficiently track nullifier creation by retrieving only the nullifiers produced within a specific range of blocks.

Caller specifies the `prefix_len` (currently only 16), the list of prefix values (`nullifiers`), and the block
range (`block_from`, optional `block_to`). The response includes all matching nullifiers created within that
range, the last block included in the response (`resp.block_num`), and the current chain tip (`chain_tip`).

If the response is chunked (i.e., `resp.block_num < block_to`), continue by issuing another request with
`block_from = block_num + 1` to retrieve subsequent updates.

---

### SyncAccountVault

Returns information that allows clients to sync asset values for specific public accounts within a block range.

For any `block_range`, the latest known set of assets is returned for the requested account ID.
The data can be split and a cutoff block may be selected if there are too many assets to sync. The response contains
the chain tip so that the caller knows when it has been reached.

---

### SyncNotes

Returns info which can be used by the client to sync up to the tip of chain for the notes they are interested in.

Client specifies the `note_tags` they are interested in, and the block range from which to search for matching notes. The request will then return the next block containing any note matching the provided tags within the specified range.

The response includes each note's metadata and inclusion proof.

A basic note sync can be implemented by repeatedly requesting the previous response's block until reaching the tip of the chain.

---

### SyncState

Returns info which can be used by the client to sync up to the latest state of the chain for the objects (accounts,
notes, nullifiers) the client is interested in.

This request returns the next block containing requested data. It also returns `chain_tip` which is the latest block
number in the chain. Client is expected to repeat these requests in a loop until
`response.block_header.block_num == response.chain_tip`, at which point the client is fully synchronized with the chain.

Each request also returns info about new notes, nullifiers etc. created. It also returns Chain MMR delta that can be
used to update the state of Chain MMR. This includes both chain MMR peaks and chain MMR nodes.

For preserving some degree of privacy, note tags and nullifiers filters contain only high part of hashes. Thus, returned
data contains excessive notes and nullifiers, client can make additional filtering of that data on its side.

---

### SyncStorageMaps

Returns storage map synchronization data for a specified public account within a given block range. This method allows clients to efficiently sync the storage map state of an account by retrieving only the changes that occurred between two blocks.

Caller specifies the `account_id` of the public account and the block range `block_range` for which to retrieve storage updates. The response includes all storage map key-value updates that occurred within that range, along with the last block included in the sync and the current chain tip.

This endpoint enables clients to maintain an updated view of account storage.

---

## License

This project is [MIT licensed](../../LICENSE).
