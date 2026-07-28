# syntax=docker/dockerfile:1

ARG RUST_VERSION=1.96
ARG DEBIAN_RELEASE=bookworm
ARG BIN
ARG PORT

FROM rust:${RUST_VERSION}-slim-${DEBIAN_RELEASE} AS builder
ARG BIN
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
WORKDIR /app
# Automatic per-platform arg (no manual wiring needed for multi-platform
# buildx builds). Used, together with BIN, to key the cache mounts below so
# that amd64 and arm64 builds never share compiled objects or checkouts —
# artifacts from one architecture cannot be reused for another. The compiled
# /app/target mount is additionally keyed by BIN: different binaries in this
# workspace enable different feature sets on shared deps, so giving each
# binary its own target dir avoids matrix builds serializing on (and
# invalidating) a shared, lock-guarded mount for artifacts they can't reuse
# anyway.
#
# The registry/git-db mounts are also keyed by BIN, even though raw sources
# have no feature dependence. Sharing one registry mount across concurrent
# matrix jobs corrupts it: Cargo permits concurrent downloads across
# processes, and two cargo processes extracting the same newly-downloaded
# crate race on creating `.cargo-ok` — the loser fails the whole build with
# "failed to open .cargo-ok ... File exists". Keying by BIN removes all
# concurrency on a registry mount: jobs for different binaries use different
# mounts, and builds of the same binary are already serialized by the locked
# target mount below (every RUN that touches the registry also holds that
# lock). The cost is one registry copy per binary per arch.
#
# Sharing modes: the target mount is sharing=locked because the correctness
# of the touch-then-build sequence below requires that no other build can
# write artifacts into it between the touch and the build. The registry and
# git-db mounts stay sharing=shared; the BIN+arch keying plus the target
# lock already guarantee exclusive access, so BuildKit-level locking on
# them would be redundant.
ARG TARGETARCH
# Build application
COPY . .
# Cargo's fingerprinting for workspace path crates is mtime-based: a crate is
# rebuilt only if a source file is newer than the cached artifact. The
# /app/target cache mount below is shared across branches and PRs on the
# persistent builder, so its artifacts may come from source that differs from
# this build context, and any source mtime older than those artifacts (local
# checkouts, git-derived timestamps) makes Cargo silently link a stale,
# incompatible .rlib. Touch all sources to the current time so every
# workspace crate is always rebuilt; external dependencies are unaffected
# (they are fingerprinted by checksum, not mtime, and stay cached in the
# mounted target dir across builds).
#
# Cache Cargo's git DB, but leave checkout worktrees ephemeral; shared checkout
# caches are fragile when concurrent CI builds race or a build is interrupted.
#
# The touch must happen inside the locked RUN below, after the target cache
# mount is acquired: a concurrent build of another branch can hold the mount
# and write artifacts into it, and a touch performed before lock acquisition
# would leave those artifacts newer than our sources, making Cargo treat them
# as fresh. Touching while holding the lock guarantees sources are newer than
# anything already in the cache. The mounted ./target is pruned from the walk
# so cached fingerprints and artifacts keep their original mtimes.
# An interrupted build (e.g. a cancelled CI job) can leave a partially
# extracted crate in the cached registry: the source dir exists but
# `.cargo-ok` is missing or empty. Cargo does not recover from this — every
# subsequent build fails with "failed to open .cargo-ok ... File exists" —
# so drop any such partial extraction before building.
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
