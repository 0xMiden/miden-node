# Configuration and Usage

As outlined in the [Architecture](./architecture.md) chapter, the node consists of several components which can be run
separately or as a single bundled process. At present, the recommended way to operate a node is in bundled mode and is
what this guide will focus on. Operating the components separately is very similar and should be relatively
straight-forward to derive from these instructions.

This guide focuses on basic usage. To discover more advanced options we recommend exploring the various help menus
which can be accessed by appending `--help` to any of the commands.

## Bootstrapping

The first step in starting a new Miden network is to initialize the genesis block data. This is a one-off operation. It can either be done through the default of generating a single account and genesis with default values:

```sh
# Create a folder to store the node's data.
mkdir data

# Bootstrap the node.
#
# This creates the node's database and initializes it with the genesis data.
#
# The genesis block currently contains a single public faucet account. The
# secret for this account is stored in the `<accounts-directory/account.mac>`
# file. This file is not used by the node and should instead by used wherever
# you intend to operate this faucet account.
#
# For example, you could operate a public faucet using our faucet reference
# implementation whose operation is described in a later section.
miden-node bundled bootstrap \
  --data-directory data \
  --accounts-directory .
```

Alternatively, provide an explicit toml-formatted configuration format.

```sh
miden-node bundled bootstrap \
  --data-directory data \
  --accounts-directory . \
  --genesis-config-file genesis.toml
```

containing i.e.

```toml
timestamp = 1717344256
version   = 1

[[faucet]]
decimals     = 12
max_supply   = 10000
storage_mode = "public"
symbol       = "FUZZY"

[[wallet]]
assets       = [{ amount = 999, symbol = "FUZZY" }]
storage_mode = "private"
can_be_updated = false # default value
```

## Operation

Start the node with the desired public gRPC server address.

```sh
miden-node bundled start \
  --data-directory data \
  --rpc.url http://0.0.0.0:57291
```

## Faucet

We also provide a reference implementation for a public faucet app with a basic webinterface to request
tokens. The app requires a faucet account file which it can either generate (for a new account), or it can use an
existing one e.g. one created as part of the genesis block.

Create a faucet account for the faucet app to use - or skip this step if you already have an account file.

Note that we specify a distinct account filename (`faucet.mac`) to avoid collision with the account file that the node bootstrap command generates.

```sh
miden-faucet create-faucet-account \
  --token-symbol BTC \
  --decimals 8 \
  --max-supply 2100000000000000 \
  --output faucet.mac
```

Run the faucet:

```sh
miden-faucet start \
  --endpoint http://127.0.0.1:8080 \
  --node-url http://127.0.0.1:57291 \
  --account faucet.mac
```

## Systemd

Our [Debian packages](./installation.md#debian-package) install a systemd service which operates the node in `bundled`
mode. You'll still need to run the [bootstrapping](#bootstrapping) process before the node can be started.

You can inspect the service file with `systemctl cat miden-node` (and `miden-faucet`) or alternatively you can see it in
our repository in the `packaging` folder. For the bootstrapping process be sure to specify the data-directory as
expected by the systemd file. If you're operating a faucet from an account generated in the genesis block, then you'll
also want to specify the accounts directory as expected by the faucet service file. With the default unmodified service
files this would be:

```sh
miden-node bundled bootstrap \
  --data-directory /opt/miden-node \
  --accounts-directory /opt/miden-faucet
```

## Environment variables

Most configuration options can also be configured using environment variables as an alternative to providing the values
via the command-line. This is useful for certain deployment options like `docker` or `systemd`, where they can be easier
to define or inject instead of changing the underlying command line options.

These are especially convenient where multiple different configuration profiles are used. Write the environment
variables to some specific `profile.env` file and load it as part of the node command:

```sh
source profile.env && miden-node <...>
```

This works well on Linux and MacOS, but Windows requires some additional scripting unfortunately.

See the `.env` files in each of the binary crates' [directories](https://github.com/0xMiden/miden-node/tree/next/bin) for a list of all available environment variables.
