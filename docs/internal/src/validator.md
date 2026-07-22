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
shared transaction encryption keypair, an Ed25519 key that miden-crypto uses for X25519 key
agreement in its IES scheme. Clients will use it to encrypt the private transaction inputs they
submit, so that any validator in the set can decrypt them.

The `GetTransactionEncryptionKey` endpoint returns the shared public key together with an IES
scheme identifier, an opaque key ID, and a list of validator attestations, currently holding one
signature from this validator's own signing key over an attestation commitment (the
`TransactionEncryptionKey` proto message documents the exact payload). The commitment carries a domain tag that separates attestations from block header
signatures, and the genesis commitment so an attestation cannot replay across networks. The
signature proves to clients that a chain-recognized validator vouches for the key, so the key can
be served through an untrusted RPC.

The key can be rotated. Operators configure every validator with the same next shared secret and a
rotation block number, and the endpoint announces the upcoming key in the `next_key` field ahead of
the rotation. The announcement is covered by the attestation commitment, so it cannot be stripped
or altered without invalidating the signatures. Once the chain tip reaches the rotation block, the
validator serves the announced key as the current one under a fresh attestation computed at
startup. The decrypter keeps both secrets, so submissions sealed against either key around the
rotation still decrypt.

This scheme does not protect the inputs from parties holding the shared secret and has no forward
secrecy. It is the first phase of the transaction input encryption design: later phases move the
key material to threshold and TEE-managed setups.
