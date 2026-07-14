# syntax=docker/dockerfile:1.7

FROM rust:1.91-bookworm AS builder

WORKDIR /work
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/work/target,sharing=locked \
    cargo build --release --locked -p listing-monitor \
    --bin listing-monitor-alpha \
    --bin listing-monitor-spot \
    && install -D -m 0755 target/release/listing-monitor-alpha /out/listing-monitor-alpha \
    && install -D -m 0755 target/release/listing-monitor-spot /out/listing-monitor-spot

FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 appuser

COPY --from=builder /out/listing-monitor-alpha /usr/local/bin/listing-monitor-alpha
COPY --from=builder /out/listing-monitor-spot /usr/local/bin/listing-monitor-spot

USER appuser

CMD ["/usr/local/bin/listing-monitor-alpha"]
