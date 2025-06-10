FROM rust:1.87-slim-bullseye AS builder

WORKDIR /app
COPY . .

RUN cargo install --path bin/faucet --locked

# Connect to the node using `host.docker.internal`. This assumes that the node is running on the 
# same host machine.
RUN miden-faucet init \
    --faucet-account-path faucet.mac \
    --config-path miden-faucet.toml \
    --node-url http://host.docker.internal:57291

RUN miden-faucet create-faucet-account \
    --output-path faucet.mac \
    --token-symbol POL \
    --decimals 9 \
    --max-supply 1000000000

LABEL org.opencontainers.image.authors=devops@miden.team \
    org.opencontainers.image.url=https://0xMiden.github.io/ \
    org.opencontainers.image.documentation=https://github.com/0xMiden/miden-node \
    org.opencontainers.image.source=https://github.com/0xMiden/miden-node \
    org.opencontainers.image.vendor=Miden \
    org.opencontainers.image.licenses=MIT

ARG CREATED
ARG VERSION
ARG COMMIT
LABEL org.opencontainers.image.created=$CREATED \
    org.opencontainers.image.version=$VERSION \
    org.opencontainers.image.revision=$COMMIT

# Expose faucet port
EXPOSE 8080

# Start the Miden faucet
# Miden faucet does not spawn sub-processes, so it can be used as the PID1
CMD miden-faucet start
