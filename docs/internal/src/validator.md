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
transaction encryption provider. The provider owns its opaque key IDs and secret storage. It
exposes public metadata for the current key and an optional manually selected next key, and it
decrypts using the key ID supplied by the caller. It does not expose raw secret bytes.

A scheduled key may activate only at an epoch boundary. Before that boundary its ID is premature.
At the boundary it becomes current and the prior current key remains decrypt-only through the
activation epoch. The old ID is expired from the following epoch boundary onward. The provider,
not the validator service, enforces these rules. The validator does not derive keys or choose an
automatic rotation policy. Providers keep the schedule fixed within each epoch. Operators publish
a manually selected next key only at an epoch boundary.

`GetTransactionEncryptionKey` returns the current key and optional next key as one schedule. A
single validator signature binds the complete schedule, including both activation blocks and the
presence or absence of the next key. It also binds the genesis commitment and an attestation
epoch. The validator lazily refreshes this attestation at most once per epoch without changing
the provider schedule.

The canonical typed verifier lives in
`miden_node_proto::domain::transaction_encryption`. It checks the signature against
chain-recognized validator keys, requires the attestation epoch to match the trusted chain tip,
and enforces activation boundaries. This lets node-owned clients reject cross-network, stale,
premature, and structurally altered schedules served through an untrusted RPC.

This scheme does not protect the inputs from parties holding the shared secret and has no forward
secrecy. It is the first phase of the transaction input encryption design: later phases move the
key material to threshold and TEE-managed setups.
