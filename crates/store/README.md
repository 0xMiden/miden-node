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
- [CheckNullifiersByPrefix](#checknullifiersbyprefix)
- [GetAccountDetails](#getaccountdetails)
- [GetAccountProofs](#getaccountproofs)
- [GetAccountStateDelta](#getaccountstatedelta)
- [GetBlockByNumber](#getblockbynumber)
- [GetBlockHeaderByNumber](#getblockheaderbynumber)
- [GetBlockInputs](#getblockinputs)
- [GetNoteAuthenticationInfo](#getnoteauthenticationinfo)
- [GetNotesById](#getnotesbyid)
- [GetTransactionInputs](#gettransactioninputs)
- [SyncNotes](#syncnotes)
- [SyncState](#syncstate)
<!--toc:end-->

---

### ApplyBlock

Applies changes of a new block to the DB and in-memory data structures. Raw block data is also stored as a flat file.

---

### CheckNullifiers

Returns a nullifier proof for each of the requested nullifiers.

---

### CheckNullifiersByPrefix

Returns a list of nullifiers that match the specified prefixes and are recorded in the node.

Only 16-bit prefixes are supported at this time.

---

### GetAccountDetails

Returns the latest state of an account with the specified ID.

---

### GetAccountProofs

Returns the latest state proofs of the specified accounts.

---

### GetAccountStateDelta

Returns delta of the account states in the range from `from_block_num` (exclusive) to `to_block_num` (inclusive).

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

### SyncNotes

Returns info which can be used by the client to sync up to the tip of chain for the notes they are interested in.

Client specifies the `note_tags` they are interested in, and the block height from which to search for new for matching
notes for. The request will then return the next block containing any note matching the provided tags.

The response includes each note's metadata and inclusion proof.

A basic note sync can be implemented by repeatedly requesting the previous response's block until reaching the tip of
the chain.

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

## License

This project is [MIT licensed](../../LICENSE).
