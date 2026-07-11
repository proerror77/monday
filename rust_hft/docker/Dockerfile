FROM rust:1.91-bookworm AS builder

WORKDIR /work
RUN apt-get update && apt-get install -y --no-install-recommends \
    clang \
    cmake \
    mold \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

COPY . .
RUN cargo --config 'build.rustc-wrapper=""' build \
    --release \
    --locked \
    -p hft-live \
    --features "clickhouse,redis"

FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 1000 hft

WORKDIR /app
COPY --from=builder /work/target/release/hft-live /usr/local/bin/hft-live
RUN mkdir -p /app/config /app/deployment /app/logs /app/models /app/state \
    && chown -R hft:hft /app

USER hft
ENV HFT_ENV=production
ENV RUST_LOG=info

EXPOSE 9090 9091 9092
HEALTHCHECK --interval=30s --timeout=10s --start-period=10s --retries=3 \
    CMD curl --fail --silent http://127.0.0.1:9090/readiness || exit 1

ENTRYPOINT ["/usr/local/bin/hft-live"]
CMD ["--config", "/app/config/system.yaml"]
