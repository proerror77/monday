## Minimal multi-stage Dockerfile for k8s deployment
FROM rust:1.80-bullseye AS builder
WORKDIR /work
COPY . .
RUN apt-get update && apt-get install -y pkg-config libssl-dev && \
    cd apps/collector && cargo build --release

FROM debian:bullseye-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /root
COPY --from=builder /work/apps/collector/target/release/hft-collector /root/hft-collector
ENTRYPOINT ["/root/hft-collector"]

