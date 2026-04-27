## Minimal multi-stage Dockerfile for collector deployment
FROM rust:1.91-bookworm AS builder
WORKDIR /work
COPY . .
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config clang mold && \
    rm -rf /var/lib/apt/lists/* && \
    cargo --config 'build.rustc-wrapper=""' build --release --locked -p hft-collector

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /work/target/release/hft-collector /usr/local/bin/hft-collector
ENTRYPOINT ["/usr/local/bin/hft-collector"]
