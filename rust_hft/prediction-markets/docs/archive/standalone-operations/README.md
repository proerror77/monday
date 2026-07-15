# Historical standalone PLOY operations

This directory preserves deployment, infrastructure, and credential-template
artifacts imported from the former standalone PLOY repository. They are
provenance only: do not copy, deploy, source, apply, or execute them as Monday
operations.

Current ownership is outside this archive:

- venue-neutral runtime packaging lives in `rust_hft/deployment`;
- Aliyun ECS, ACK, OSS, systemd, and health-control assets live in
  `deployment/aliyun`;
- venue credentials, account inspection, orders, cancellation, and
  reconciliation belong to Monday's canonical execution runtime.

Files below may contain placeholder private-key names, legacy host paths,
standalone risk settings, or Terraform/Kubernetes examples. Their presence is
not deployment approval and they are excluded from active runtime entrypoints.

The `docker/` directory contains the former standalone runner, collector, and
research image definitions plus their build-context ignore file. Monday image
ownership remains under `rust_hft/deployment`; these archived Dockerfiles must
not be passed to an active build or publish workflow.
