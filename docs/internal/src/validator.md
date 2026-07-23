# Validator Component

The validator is responsible for verifying each new block and signing it if correct.

This signature is required _before_ a block may be committed on chain, and thus acts as an
independent safe guard.

The validator is therefore run completely separate from the main node operations, and is operated
by a separate entity. The validator's public key is published (or at least will be for `mainnet`).

## Dual purpose: training wheels

The validator has a 2nd purpose while Miden is maturing. To prevent private state from being lost, and to guard
from potential bugs in the VM/cryptography primitives, Miden will launch with training wheels. Notably, we will
require users to _include_ the private input data along with their transactions. This means users will have privacy
on the _network_ but not from the network operator.

As part of the transaction submission process, each transaction, its proof, and private inputs, are sent to the validator,
which re-executes the transaction, thereby verifying it and its proof are correct. This also lets us store the private data
as part of our training wheels.

## Block verification

The validator ensures that each new block is sequential with the previously signed block. i.e. `header.parent_commitment == last_block.commitment`.
It also checks that the block contains only transactions that it has previously seen and verified.

Once verified, the block is signed and returned to the sender.

## Transaction encryption key

In addition to its per-validator signing key, every validator is provisioned with the _same_
shared master secret for transaction encryption. From it, the validator derives one encryption
keypair per epoch (an Ed25519 key that miden-crypto uses for X25519 key agreement in its IES
scheme): the key seed is the blake3 hash of a domain tag, the master secret, and the epoch
number. Clients will use the epoch's key to encrypt the private transaction inputs they submit,
so that any validator in the set can decrypt them.

The encryption key rotates at every epoch boundary (every `2^16` blocks). Since the derivation
is deterministic, all validators transition to the same new key without coordination. A
background task follows the validator's committed chain tip and, after each boundary, derives
the new epoch's keys and re-signs their attestations off the request path. Submissions sealed
against the previous epoch's key remain decryptable for one further epoch as a grace window.

Each epoch's secret key is archived in the validator's database, at startup (backfilling any
epochs missed while offline) and at every rotation, always including the announced next epoch's
key since clients near a boundary may already seal against it. The archive preserves the key
material needed to recover past submissions should the shared master secret ever be replaced;
the live decrypt path always re-derives from the master secret. Decrypter implementations that
cannot export secret key bytes (e.g. a future TEE-held key) skip this archival and are
responsible for their own.

The `GetTransactionEncryptionKey` endpoint returns the current key and the key that replaces it
at the next epoch boundary, each carrying an IES scheme identifier, an opaque key ID, and a list
of validator attestations, currently holding one signature from this validator's own signing key
over an attestation commitment (the `ValidatorKeyAttestation` proto message documents the exact
payload). The commitment carries a domain tag that separates attestations from block header
signatures, the genesis commitment so an attestation cannot replay across networks, and a role
suffix that separates current-key from next-key attestations and binds the next key's rotation
block. The signature proves to clients that a chain-recognized validator vouches for the key, so
the key can be served through an untrusted RPC.

This scheme does not protect the inputs from parties holding the shared secret and has no forward
secrecy. It is the first phase of the transaction input encryption design: later phases move the
key material to threshold and TEE-managed setups.
