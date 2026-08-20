# syntax=docker/dockerfile:1

ARG RUST_VERSION=1.96
ARG DEBIAN_RELEASE=bookworm
ARG BIN
ARG PORT
# Builder stage the runtime binary is copied from: `builder-ci` compiles one
# binary per image (concurrent CI matrix builds), `builder-local` compiles
# every binary in one invocation (sequential local builds).
ARG BUILDER=builder-ci

FROM rust:${RUST_VERSION}-slim-${DEBIAN_RELEASE} AS build-base
# Disable incremental compilation: its reuse depends on the same mtime-based
# fingerprinting that the shared CI target cache cannot make safe (see
# builder-ci below), and release builds gain nothing from it anyway. Dep
# .rlibs in the /app/target cache mount still accelerate builds.
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
WORKDIR /app

FROM build-base AS builder-ci
ARG BIN
# All cache mounts are keyed by BIN and TARGETARCH. The arch keying keeps
# amd64/arm64 artifacts apart. The BIN keying serves two purposes: for the
# target mount, each binary enables different feature sets on shared deps, so
# a shared locked mount would only serialize matrix builds on artifacts they
# cannot reuse; for the registry/git-db mounts, two concurrent cargo processes
# extracting the same crate race on creating `.cargo-ok`, and the loser fails
# the build ("failed to open .cargo-ok ... File exists") — keying by BIN
# removes all concurrency on a mount, since builds of the same binary are
# already serialized by the locked target mount. The cost is one registry
# copy per binary per arch.
#
# The target mount is sharing=locked because the touch-then-build sequence
# below must not interleave with another build's writes; the registry/git-db
# mounts stay sharing=shared since the keying already guarantees exclusivity.
ARG TARGETARCH
COPY . .
# Cargo fingerprints workspace path crates by mtime: a crate is rebuilt only
# if a source file is newer than the cached artifact. The /app/target mount is
# shared across branches and PRs on the persistent builder, so a source mtime
# older than another build's artifacts (warm checkouts, git-derived
# timestamps) makes Cargo silently link a stale, incompatible .rlib. Touch all
# sources so workspace crates always rebuild; external deps are fingerprinted
# by checksum and stay cached. The touch must happen after the locked target
# mount is acquired — a concurrent build of another branch could otherwise
# write artifacts newer than our sources after we touch them — and prunes
# ./target from the walk so cached artifacts keep their mtimes.
#
# Cargo's git DB is cached but checkout worktrees stay ephemeral; shared
# checkouts are fragile under concurrent or interrupted builds.
#
# An interrupted build can leave a partially extracted crate in the cached
# registry (source dir present, `.cargo-ok` missing or empty). Cargo never
# recovers from this, so drop any such partial extraction before building.
RUN --mount=type=cache,sharing=shared,id=cargo-registry-${BIN}-${TARGETARCH},target=/usr/local/cargo/registry \
    --mount=type=cache,sharing=shared,id=cargo-git-${BIN}-${TARGETARCH},target=/usr/local/cargo/git/db \
    --mount=type=cache,sharing=locked,id=app-target-${BIN}-${TARGETARCH},target=/app/target \
    if [ -d /usr/local/cargo/registry/src ]; then \
        find /usr/local/cargo/registry/src -mindepth 2 -maxdepth 2 -type d \
            '!' -exec test -s '{}/.cargo-ok' ';' -exec rm -rf '{}' +; \
    fi && \
    find . -path ./target -prune -o -type f -exec touch {} + && \
    cargo build --release --locked --bin ${BIN} && \
    mkdir -p /app/bin && \
    cp /app/target/release/${BIN} /app/bin/${BIN}

# Local builder: compiles every image's binary in one cargo invocation. Local
# images are built sequentially on one machine, so the per-BIN mount keying
# above would only multiply work (each binary compiling its own copy of the
# dependency tree, including RocksDB). This stage never references BIN, so its
# layers are identical across all image builds: the first build compiles and
# the rest hit cache.
#
# Sources are not touched here: a local context preserves real mtimes and git
# updates the mtime of anything it changes, so Cargo's fingerprinting is sound
# and rebuilds are genuinely incremental. The `.cargo-ok` heal is kept — an
# interrupted (Ctrl-C) build corrupts the cached registry just like in CI.
#
# Parallelism is capped at ~2GiB of memory per job: the release profile
# carries full debug info, and an uncapped build OOMs the default ~8GiB
# Docker Desktop VM. Pass CARGO_BUILD_JOBS to override.
FROM build-base AS builder-local
ARG TARGETARCH
ARG CARGO_BUILD_JOBS
COPY . .
RUN --mount=type=cache,sharing=shared,id=cargo-registry-local-${TARGETARCH},target=/usr/local/cargo/registry \
    --mount=type=cache,sharing=shared,id=cargo-git-local-${TARGETARCH},target=/usr/local/cargo/git/db \
    --mount=type=cache,sharing=locked,id=app-target-local-${TARGETARCH},target=/app/target \
    if [ -d /usr/local/cargo/registry/src ]; then \
        find /usr/local/cargo/registry/src -mindepth 2 -maxdepth 2 -type d \
            '!' -exec test -s '{}/.cargo-ok' ';' -exec rm -rf '{}' +; \
    fi && \
    JOBS="${CARGO_BUILD_JOBS:-$(awk -v ncpu="$(nproc)" \
        '/MemTotal/ { j = int($2 / (2 * 1024 * 1024)); if (j < 1) j = 1; if (j > ncpu) j = ncpu; print j }' \
        /proc/meminfo)}" && \
    cargo build --release --locked --jobs "$JOBS" \
        --bin miden-node \
        --bin miden-validator \
        --bin miden-ntx-builder \
        --bin miden-network-monitor \
        --bin miden-remote-prover \
        --bin miden-benchmark && \
    mkdir -p /app/bin && \
    cp /app/target/release/miden-node \
        /app/target/release/miden-validator \
        /app/target/release/miden-ntx-builder \
        /app/target/release/miden-network-monitor \
        /app/target/release/miden-remote-prover \
        /app/target/release/miden-benchmark \
        /app/bin/

# Alias stage so the runtime COPY below can select a builder via build arg.
FROM ${BUILDER} AS build-result

# Baseline runtime image with runtime dependencies installed.
FROM debian:${DEBIAN_RELEASE}-slim AS runtime-base
RUN apt-get update && \
    apt-get -y upgrade && \
    apt-get install -y --no-install-recommends \
        ca-certificates && \
    rm -rf /var/lib/apt/lists/*
# Unprivileged runtime user. `/data` is created here so a first-use named
# volume mounted at `/data` inherits this ownership (Docker copies the image
# directory into a new named volume). Without that, the volume is root:root
# and the process cannot write.
RUN groupadd --gid 10001 miden && \
    useradd --uid 10001 --gid miden --no-create-home --home-dir /nonexistent \
        --shell /usr/sbin/nologin miden && \
    mkdir -p /data && \
    chown miden:miden /data

FROM runtime-base AS runtime-common
ARG BIN
COPY --from=build-result /app/bin/${BIN} /usr/local/bin/${BIN}
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
# Use exec to replace the shell so the binary runs as PID 1.
ENV MIDEN_BIN=${BIN}
CMD ["/bin/sh", "-c", "exec /usr/local/bin/$MIDEN_BIN"]
USER miden

# Command-line tools do not listen on a port.
FROM runtime-common AS runtime-tool

# Keep the default final target for the network's long-running services.
FROM runtime-common AS runtime
ARG PORT
EXPOSE ${PORT}
