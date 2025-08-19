FROM rust:1.88-slim-bullseye AS builder

WORKDIR /app
COPY ./Cargo.toml .
COPY ./Cargo.lock .
COPY ./bin ./bin
COPY ./crates ./crates
COPY ./proto ./proto

RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    build-essential

RUN cargo install --path bin/faucet --locked

RUN miden-faucet create-faucet-account \
    --output faucet.mac \
    --token-symbol MIDEN \
    --decimals 6 \
    --max-supply 100000000000000000

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
