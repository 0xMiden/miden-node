# Miden validator

`miden-validator` is a Miden node binary that validates network activity before blocks are committed. It is part of the
Miden node repository; see the [repository README](https://github.com/0xMiden/node#readme) for the overall project
layout.

## Role

The validator is separate from `node` so that block construction and block validation can be operated as distinct
services. It verifies submitted transactions, validates proposed blocks, and signs blocks that satisfy the validator's
checks.

The validator binary is also responsible for creating the genesis block, via its `genesis` command. The genesis block is
not signed; it commits to the validator set that must sign every subsequent block, and is then used to initialize the
validators, the node, and other services that need trusted genesis state.

## Operation

The validator expects to operate as an internal service within a Miden network's infrastructure and exposes a gRPC API
for use by trusted internal nodes.

It supports local development keys and KMS-backed signing for deployments that need external key management.

Validator databases created before protocol configuration storage was added are not compatible with this version. Keep
the old database as a backup, then bootstrap a new validator data directory from the trusted genesis file. There is no
migration or import step for the old validator database.

## License

This project is [MIT licensed](../../LICENSE).
