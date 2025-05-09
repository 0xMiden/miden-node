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
- [GetAccountStateDelta](#getaccountstatedelta)
- [GetBlockByNumber](#getblockbynumber)
- [GetBlockHeaderByNumber](#getblockheaderbynumber)
- [GetNotesById](#getnotesbyid)
- [SubmitProvenTransaction](#submitproventransaction)
- [SyncNotes](#syncnotes)
- [SyncState](#syncstate)

<!--toc:end-->

## CheckNullifiers

Request proofs for a set of nullifiers.

## CheckNullifiersByPrefix

Request nullifiers filtered by prefix and created after some block number.

The prefix is used to obscure the callers interest in a specific nullifier. Currently only 16-bit prefixes are
supported.

## GetAccountDetails

Request the latest state of an account.

## GetAccountProofs

Request state proofs for accounts, including specific storage slots.

## GetAccountStateDelta

Request the delta of an account's state for a range of blocks. This can be used to update your local account state to
the latest network state.

## GetBlockByNumber

Request the raw data for a specific block.

## GetBlockHeaderByNumber

Request a specific block header and its inclusion proof.

## GetNotesById

Request a set of notes.

## SubmitProvenTransaction

Submit a transaction to the network.

## SyncNotes

Iteratively sync data for a given set of note tags.

Client specify the note tags of interest and the block height from which to search. The response returns the next block
containing note matching the provided tags.

The response includes each note's metadata and inclusion proof.

A basic note sync can be implemented by repeatedly requesting the previous response's block until reaching the tip of
the chain.

## SyncState

Iteratively sync data for specific notes and accounts.

This request returns the next block containing data of interest. number in the chain. Client is expected to repeat these
requests in a loop until the reponse reaches the head of the chain, at which point the data is fully synced.

Each update response also contains info about new notes, accounts etc. created. It also returns Chain MMR delta that can
be used to update the state of Chain MMR. This includes both chain MMR peaks and chain MMR nodes.

The low part of note tags are redacted to preserve some degree of privacy. Returned data therefore contains additional
notes which should be filtered out by the client.
