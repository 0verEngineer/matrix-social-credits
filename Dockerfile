# Build stage
FROM rust:1.72.1 AS builder

# Set up a working directory
WORKDIR /usr/src/matrix-social-credits

COPY ./src ./src
COPY Cargo.toml Cargo.lock ./

RUN cargo build --release

# Final stage
FROM rust:1.72.1

COPY --from=builder /usr/src/matrix-social-credits/target/release/matrix-social-credits /usr/local/bin/

CMD ["matrix-social-credits"]

