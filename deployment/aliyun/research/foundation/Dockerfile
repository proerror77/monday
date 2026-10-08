# Prebuilt control image only. Caller supplies a reviewed digest-pinned runtime
# with libc/libgcc, git, gh and CA support. No source checkout or Cargo in this image.
ARG RUNTIME_IMAGE
FROM ${RUNTIME_IMAGE}
ARG SOURCE_REVISION
LABEL org.opencontainers.image.revision=${SOURCE_REVISION}
COPY --chmod=0555 rust_hft/research-control-bin/research-orchestrator /usr/local/bin/
COPY --chmod=0555 rust_hft/research-control-bin/research-artifact-gateway /usr/local/bin/
COPY --chmod=0555 rust_hft/research-control-bin/researchctl /usr/local/bin/
# Copy only the authenticated controller release artifact; never the CI debug issuer.
# Existing controlled ACK capacity executes independent release import.
COPY --chmod=0555 rust_hft/research-control-bin/research-release-publisher /usr/local/bin/
COPY --chmod=0555 rust_hft/research-control-bin/research-session /usr/local/bin/
USER 10001:10001
WORKDIR /
