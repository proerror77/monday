# Multi-stage Dockerfile for hft-live app

FROM rust:1.77-slim as builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /work
COPY . .

# Build only the hft-live binary with required features
RUN cargo build -p hft-live --release --features "bitget,binance,imbalance-strategy,full-infra"

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
RUN useradd -r -s /bin/false hftuser && mkdir -p /app/config && chown -R hftuser:hftuser /app
COPY --from=builder /work/target/release/hft-live /usr/local/bin/hft-live
USER hftuser
WORKDIR /app

# Metrics port
EXPOSE 9090

# Default command expects an external config mounted at /app/config/system.yaml
CMD ["/usr/local/bin/hft-live", "--config", "/app/config/system.yaml"]

