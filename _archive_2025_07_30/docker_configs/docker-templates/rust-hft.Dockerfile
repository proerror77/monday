# Multi-stage Dockerfile for Rust HFT Core Engine
# 針對HFT系統優化的Rust Docker映像

# Stage 1: Build environment
FROM rust:1.75-bullseye as builder

WORKDIR /build

# Install system dependencies for low-latency networking
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    librdkafka-dev \
    protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

# Copy Cargo files
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY build.rs ./

# Build with optimizations for production
RUN cargo build --release

# Stage 2: Runtime environment
FROM debian:bullseye-slim as production

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Create app user for security
RUN useradd -r -s /bin/false hftuser

WORKDIR /app

# Copy binary from builder stage
COPY --from=builder /build/target/release/rust_hft ./rust_hft
COPY --from=builder /build/target/release/health-check ./health-check

# Create necessary directories
RUN mkdir -p logs data models config && \
    chown -R hftuser:hftuser /app

# Copy configuration templates
COPY config/ ./config/

USER hftuser

# Health check
HEALTHCHECK --interval=30s --timeout=10s --start-period=60s --retries=3 \
    CMD ./health-check

# Expose ports
EXPOSE 50051 8080

# Run the application
CMD ["./rust_hft"]