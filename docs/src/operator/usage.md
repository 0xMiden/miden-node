# Configuration and Usage

As outlined in the [Architecture](./architecture.md) chapter, the node consists of several components which can be run
separately or as a single bundled process. At present, the recommended way to operate a node is in bundled mode and is
what this guide will focus on. Operating the components separately is very similar and should be relatively
straight-foward to derive from these instructions.

This guide focusses on basic usage. To discover more advanced options we recommend exploring the various help menus
which can be accessed by appending `--help` to any of the commands.

## Bootstrapping

The first step in starting a new Miden network is to initialize the genesis block data. This is a once-off operation.

```sh
# Create a folder to store the node's data.
mkdir data 

# Create a folder to store the genesis block's account secrets and data.
#
# These can be used to access the accounts afterwards.
# Without these the accounts would be inaccessible.
mkdir accounts

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
  --accounts-directory accounts \
```

## Operation

Start the node with the desired public gRPC server address.

```sh
miden-node bundled start \
  --data-directory data \
  --rpc.url http://0.0.0.0:57123
```

## Faucet

We also provide a reference implementation for a public faucet app with a basic webinterface to request
tokens. The app requires a faucet account file which it can either generate (for a new account), or it can use an
existing one e.g. one created as part of the genesis block.

Create a faucet account for the faucet app to use - or skip this step if you already have an account file.

```sh
mkdir accounts
miden-faucet create-faucet-account \
  --token-symbol MY_TOKEN \
  --decimals 12 \
  --max-supply 5000
```

Create a configuration file for the faucet.  

```sh
# This generates `miden-faucet.toml` which is used to configure the faucet.
#
# You can inspect and modify this if you want to make changes
# e.g. to the website url.
miden-faucet init \
  --config-path miden-faucet.toml \
  --faucet-account-path accounts/account.mac 
```

Run the faucet:

```sh
miden-faucet --config miden-faucet.toml
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
