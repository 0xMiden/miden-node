#!/bin/sh

set -e

# Check
credentials=~/.cargo/credentials.toml
if [ ! -f "$credentials" ]; then
    red="\033[0;31m"
    echo "${red}Error: $credentials not found.
see https://doc.rust-lang.org/cargo/reference/publishing.html."
    exit 1
fi

# Checkout
echo "Checking out main branch..."
git checkout main
git pull origin main

# Publish
echo "Publishing crates..."
crates=(
miden-node-utils
miden-node-proto-build
miden-node-proto
miden-node-rpc
miden-node-store
miden-node-block-producer
miden-node-ntx-builder
miden-node
miden-faucet
)
for crate in ${crates[@]}; do
    echo "Publishing $crate..."
    cargo publish -p "$crate"
done
