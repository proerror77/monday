# Multi-stage Dockerfile for hft-collector

FROM rust:1.88-bullseye AS builder
WORKDIR /app

# Install build dependencies required by collector
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev \
 && rm -rf /var/lib/apt/lists/*

# Copy workspace (use .dockerignore to trim)
COPY . .

# Remove local-only cargo overrides (mold/sccache) inside container
RUN rm -f .cargo/config.toml

# Build only the collector binary (uses crate default features)
ENV RUSTFLAGS=""
ENV RUSTC_WRAPPER=""
RUN cargo build -p hft-collector --release

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app
RUN mkdir -p /app/config
COPY --from=builder /app/target/release/hft-collector /usr/local/bin/hft-collector
COPY --from=builder /app/config/symbol_whitelist.json /app/config/symbol_whitelist.json

ENV RUST_LOG=info
ENTRYPOINT ["hft-collector"]
CMD ["--help"]
