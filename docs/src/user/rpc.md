# gRPC Reference

This is a reference of the Node's public RPC interface. It consists of a gRPC API which may be used to submit
transactions and query the state of the blockchain.

The gRPC service definition can be found in the Miden node's `proto`
[directory](https://github.com/0xMiden/miden-node/tree/main/proto) in the `rpc.proto` file.

<!--toc:start-->

- [CheckNullifiers](#checknullifiers)
- [CheckNullifiersByPrefix](#checknullifiersbyprefix)
- [GetAccountDetails](#getaccountdetails)
- [GetAccountProofs](#getaccountproofs)
- [GetBlockByNumber](#getblockbynumber)
- [GetBlockHeaderByNumber](#getblockheaderbynumber)
- [GetNotesById](#getnotesbyid)
- [SubmitProvenTransaction](#submitproventransaction)
- [SyncAccountVault](#syncaccountvault)
- [SyncNotes](#syncnotes)
- [SyncState](#syncstate)
- [SyncStorageMaps](#syncstoragemaps)
- [Status](#status)

<!--toc:end-->

## CheckNullifiers

Request proofs for a set of nullifiers.

## CheckNullifiersByPrefix

Request nullifiers filtered by prefix and created after some block number.

The prefix is used to obscure the callers interest in a specific nullifier. Currently only 16-bit prefixes are supported.

## GetAccountDetails

Request the latest state of an account.

## GetAccountProofs

Request state proofs for accounts, including specific storage slots.

## GetBlockByNumber

Request the raw data for a specific block.

## GetBlockHeaderByNumber

Request a specific block header and its inclusion proof.

## GetNotesById

Request a set of notes.

## SubmitProvenTransaction

Submit a transaction to the network.

## SyncAccountVault

Returns information that allows clients to sync asset values for specific public accounts within a block range.

For any `[block_from..block_to]` range, the latest known set of assets is returned for the requested account ID.
The data can be split and a cutoff block may be selected if there are too many assets to sync. The response contains
the chain tip so that the caller knows when it has been reached.

## SyncNotes

Iteratively sync data for a given set of note tags.

Client specify the note tags of interest and the block height from which to search. The response returns the next block containing note matching the provided tags.

The response includes each note's metadata and inclusion proof.

A basic note sync can be implemented by repeatedly requesting the previous response's block until reaching the tip of the chain.

## SyncState

Iteratively sync data for specific notes and accounts.

This request returns the next block containing data of interest. Client is expected to repeat these requests in a loop until the response reaches the head of the chain, at which point the data is fully synced.

Each update response also contains info about new notes, accounts etc. created. It also returns Chain MMR delta that can be used to update the state of Chain MMR. This includes both chain MMR peaks and chain MMR nodes.

The low part of note tags are redacted to preserve some degree of privacy. Returned data therefore contains additional notes which should be filtered out by the client.

## SyncStorageMaps

Returns storage map synchronization data for a specified public account within a given block range. This method allows clients to efficiently sync the storage map state of an account by retrieving only the changes that occurred between two blocks.

Caller specifies the `account_id` of the public account and the block range (`block_from`, `block_to`) for which to retrieve storage updates. The response includes all storage map key-value updates that occurred within that range, along with the last block included in the sync and the current chain tip.

This endpoint enables clients to maintain an updated view of account storage.

## Status

Request the status of the node components. The response contains the current version of the RPC component and the connection status of the other components, including their versions and the number of the most recent block in the chain (chain tip).

## SyncStorageMaps

Returns storage map synchronization data for a specified public account within a given block range. This method allows clients to efficiently sync the storage map state of an account by retrieving only the changes that occurred between two blocks.

Caller specifies the `account_id` of the public account and the block range (`block_from`, `block_to`) for which to retrieve storage updates. The response includes all storage map key-value updates that occurred within that range, along with the last block included in the sync and the current chain tip.

This endpoint enables clients to maintain an updated view of account storage.
