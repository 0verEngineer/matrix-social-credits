# syntax=docker/dockerfile:1

# Build stage
#
# The image tag only has to be new enough for rust-toolchain.toml; rustup installs the exact
# version pinned there in its own cached layer below.
FROM rust:1.98-trixie AS builder

WORKDIR /usr/src/matrix-social-credits

# Install the pinned toolchain first, so it is not re-downloaded on every source change.
COPY rust-toolchain.toml ./
RUN rustup show

# Build the dependencies against a stub binary. Only this layer is invalidated when
# Cargo.toml or Cargo.lock change, so editing the sources no longer recompiles matrix-sdk.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --locked \
    && rm -rf src

COPY ./src ./src
# Touch main.rs so cargo notices the real sources replaced the stub.
RUN touch src/main.rs && cargo build --release --locked

# Runtime stage
#
# The previous image used rust:1.72.1 for the runtime as well, which shipped the whole
# toolchain (roughly 1.5 GB) to run a single binary. matrix-sdk uses rustls, so the runtime
# only needs the CA bundle.
FROM debian:trixie-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Run as a normal user; the data volume is owned by it.
RUN useradd --system --create-home --uid 10001 socialcredit \
    && mkdir -p /data \
    && chown socialcredit:socialcredit /data

COPY --from=builder /usr/src/matrix-social-credits/target/release/matrix-social-credits /usr/local/bin/

USER socialcredit
VOLUME ["/data"]

ENV DB_PATH=/data/social_credit.db \
    STORE_PATH=/data/store

ENTRYPOINT ["matrix-social-credits"]
