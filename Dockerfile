# syntax=docker/dockerfile:1

ARG RUST_VERSION=1.96
ARG DEBIAN_RELEASE=bookworm
ARG BIN
ARG PORT

FROM rust:${RUST_VERSION}-slim-${DEBIAN_RELEASE} AS chef
# Disable incremental compilation: Docker normalises COPY timestamps, which
# breaks Rust's mtime-based fingerprinting and causes stale .rlib reuse.
# The /app/target cache still accelerates builds via pre-compiled dep .rlibs.
ENV CARGO_INCREMENTAL=0
# Install build dependencies. RocksDB is compiled from source by librocksdb-sys.
RUN apt-get update && \
    apt-get -y upgrade && \
    apt-get install -y --no-install-recommends \
        llvm \
        clang \
        libclang-dev \
        cmake \
        pkg-config \
        libssl-dev \
        ca-certificates && \
    rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-chef
WORKDIR /app


FROM chef AS planner
ARG BIN
COPY . .
# Scope the recipe to this binary's dependency graph. Different binaries in
# this workspace enable different, non-overlapping feature sets on shared
# deps (e.g. miden-protocol, miden-tx); preparing/cooking for the whole
# workspace would resolve a feature union that never matches what `cargo
# build --bin ${BIN}` below actually needs, forcing those crates (and
# everything downstream of them) to recompile on every build regardless of
# caching.
RUN cargo chef prepare --recipe-path recipe.json --bin ${BIN}

FROM chef AS builder
ARG BIN
# Automatic per-platform arg (no manual wiring needed for multi-platform
# buildx builds). Used, together with BIN, to key the cache mounts below so
# that amd64 and arm64 builds never share compiled objects or checkouts —
# artifacts from one architecture cannot be reused for another. The compiled
# /app/target mount is additionally keyed by BIN: different binaries in this
# workspace enable different feature sets on shared deps, so giving each
# binary its own target dir avoids matrix builds serializing on (and
# invalidating) a shared, lock-guarded mount for artifacts they can't reuse
# anyway. Registry/git-db mounts stay arch-only since raw sources have no
# feature dependence and are worth sharing across binaries.
ARG TARGETARCH
# Disable incremental compilation: Docker normalises COPY timestamps, which
# breaks Rust's mtime-based fingerprinting and causes stale .rlib reuse.
# The /app/target cache still accelerates builds via pre-compiled dep .rlibs.
ENV CARGO_INCREMENTAL=0
COPY --from=planner /app/recipe.json recipe.json
# Build dependencies while preserving Cargo artifacts across layer invalidations.
#
# Cache Cargo's git DB, but leave checkout worktrees ephemeral; shared checkout
# caches are fragile when concurrent CI builds race or a build is interrupted.
RUN --mount=type=cache,sharing=locked,id=cargo-registry-${TARGETARCH},target=/usr/local/cargo/registry \
    --mount=type=cache,sharing=locked,id=cargo-git-${TARGETARCH},target=/usr/local/cargo/git/db \
    --mount=type=cache,sharing=locked,id=app-target-${BIN}-${TARGETARCH},target=/app/target \
    cargo chef cook --release --recipe-path recipe.json --bin ${BIN}
# Build application
COPY . .
# Cargo's fingerprinting for workspace path crates is mtime-based: a crate is
# rebuilt only if a source file is newer than the cached artifact. The
# /app/target cache mount above is shared across branches and PRs on the
# persistent builder, so its artifacts may come from source that differs from
# this build context, and any source mtime older than those artifacts (local
# checkouts, git-derived timestamps) makes Cargo silently link a stale,
# incompatible .rlib. Touch all sources to the current time so every
# workspace crate is always rebuilt; external dependencies are unaffected
# (they are fingerprinted by checksum and stay cached via `cargo chef cook`).
#
# The touch must happen inside the locked RUN below, after the target cache
# mount is acquired: a concurrent build of another branch can hold the mount
# and write artifacts into it, and a touch performed before lock acquisition
# would leave those artifacts newer than our sources, making Cargo treat them
# as fresh. Touching while holding the lock guarantees sources are newer than
# anything already in the cache. The mounted ./target is pruned from the walk
# so cached fingerprints and artifacts keep their original mtimes.
#
# Diagnostics for stale-cache debugging: dump the current time and the mtimes
# of cached fingerprints and sources so "artifact newer than touched sources"
# anomalies are visible in CI logs, and have Cargo log its per-unit
# fresh/dirty verdicts.
RUN --mount=type=cache,sharing=locked,id=cargo-registry-${TARGETARCH},target=/usr/local/cargo/registry \
    --mount=type=cache,sharing=locked,id=cargo-git-${TARGETARCH},target=/usr/local/cargo/git/db \
    --mount=type=cache,sharing=locked,id=app-target-${BIN}-${TARGETARCH},target=/app/target \
    date --iso-8601=seconds && \
    (ls -la --time-style=full-iso /app/target/release/.fingerprint/ | grep miden-node-utils || true) && \
    find . -path ./target -prune -o -type f -exec touch {} + && \
    ls -la --time-style=full-iso /app/crates/utils/src/ && \
    CARGO_LOG=cargo::core::compiler::fingerprint=info \
    cargo build --release --locked --bin ${BIN} && \
    mkdir -p /app/bin && \
    cp /app/target/release/${BIN} /app/bin/${BIN}

# Baseline runtime image with runtime dependencies installed.
FROM debian:${DEBIAN_RELEASE}-slim AS runtime-base
RUN apt-get update && \
    apt-get -y upgrade && \
    apt-get install -y --no-install-recommends \
        ca-certificates && \
    rm -rf /var/lib/apt/lists/*

FROM runtime-base AS runtime
ARG BIN
ARG PORT
COPY --from=builder /app/bin/${BIN} /usr/local/bin/${BIN}
LABEL org.opencontainers.image.authors=devops@miden.team \
    org.opencontainers.image.url=https://0xMiden.github.io/ \
    org.opencontainers.image.documentation=https://github.com/0xMiden/node \
    org.opencontainers.image.source=https://github.com/0xMiden/node \
    org.opencontainers.image.vendor=Miden \
    org.opencontainers.image.licenses=MIT
ARG CREATED
ARG VERSION
ARG COMMIT
LABEL org.opencontainers.image.created=$CREATED \
    org.opencontainers.image.version=$VERSION \
    org.opencontainers.image.revision=$COMMIT
EXPOSE ${PORT}
# Use exec to replace the shell so the binary runs as PID 1.
ENV MIDEN_BIN=${BIN}
CMD ["/bin/sh", "-c", "exec /usr/local/bin/$MIDEN_BIN"]
