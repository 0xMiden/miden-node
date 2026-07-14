---
title: "Bootstrap and Genesis"
sidebar_position: 3
---

<!-- markdownlint-disable MD033 MD041 -->

import Tabs from "@theme/Tabs"; import TabItem from "@theme/TabItem";

# Bootstrap and Genesis

A signed genesis block is the trust anchor for every service that joins a network. The network's validator is
responsible for creating and signing this block. On official networks, the validator is operated by a separate entity
from the network operator.

This signed block is subsequently made available for official networks at

```text
https://genesis.<network>.miden.io
```

which provides an easy method to obtain this data. This is directly supported by service bootstrap commands by passing
`--network testnet` or `--network devnet`. Bootstrap commands also support passing a file directly to cover custom
networks, or if the official URLs are not trusted.

## Bootstrap Flow

<Tabs groupId="network-operator-genesis-source" defaultValue="official">
  <TabItem value="official" label="Official network">

The genesis block is the chain's trust root and must be signed by the complete validator set, so **one** validator
operator runs the signing form of `bootstrap` with every validator's KMS key ID (repeat the argument or comma-separate
the values):

```bash
miden-validator bootstrap \
  --data-directory validator-1-data \
  --genesis-block-directory genesis-data \
  --accounts-directory accounts \
  --genesis-config-file genesis.toml \
  --key.kms-id <validator-1-kms-key-id> \
  --key.kms-id <validator-2-kms-key-id> \
  --key.kms-id <validator-3-kms-key-id>
```

Upload `genesis-data/genesis.dat` so it is served at:

```text
https://genesis.<network>.miden.io
```

Every other validator operator seeds their own database from the same genesis block, without re-signing it:

```bash
miden-validator bootstrap \
  --data-directory validator-2-data \
  --genesis-block-directory genesis-data \
  --accounts-directory accounts \
  --file genesis-data/genesis.dat
```

Initialize the sequencer's node storage from the hosted genesis block:

```bash
miden-node bootstrap \
  --data-directory node-data \
  --network testnet
```

Initialize the network transaction builder from the same hosted genesis block:

```bash
miden-ntx-builder bootstrap \
  --data-directory ntx-builder-data \
  --network testnet
```

For `devnet`, use `--network devnet` instead. The `--network` flag is shorthand for downloading the signed genesis block
from `https://genesis.<network>.miden.io`.

Each validator operator's own KMS key ID must be used when that operator starts their validator for this network.

  </TabItem>
  <TabItem value="unofficial" label="Unofficial network">

**One** validator operator creates and signs the genesis block with every validator's local key. The genesis block is
the chain's trust root and must be signed by every member of its validator set, so pass one key per validator (repeat
the argument or comma-separate the values):

```bash
miden-validator bootstrap \
  --data-directory validator-1-data \
  --genesis-block-directory genesis-data \
  --accounts-directory accounts \
  --genesis-config-file genesis.toml \
  --key.hex <validator-1-key-hex> \
  --key.hex <validator-2-key-hex> \
  --key.hex <validator-3-key-hex>
```

Distribute `genesis-data/genesis.dat` to the other validator operators, who each seed their own database from it,
without re-signing it:

```bash
miden-validator bootstrap \
  --data-directory validator-2-data \
  --genesis-block-directory genesis-data \
  --accounts-directory accounts \
  --file genesis-data/genesis.dat
```

For unofficial networks or pre-publication testing, distribute the signed genesis block file directly and initialize
services from that file:

```bash
miden-node bootstrap \
  --data-directory node-data \
  --file genesis-data/genesis.dat
```

```bash
miden-ntx-builder bootstrap \
  --data-directory ntx-builder-data \
  --file genesis-data/genesis.dat
```

  </TabItem>
</Tabs>

The validator key used during bootstrap must match the key used when starting the validator for the network.

<!-- markdownlint-enable MD033 MD041 -->
