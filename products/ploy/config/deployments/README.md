# PLOY deployment manifest fixtures inside Monday

These JSON files are retained as PLOY control-plane and contract fixtures.

- Paper manifests may be used for focused local tests.
- Live and dry-run manifests record the former standalone topology and are not Monday
  deployment entrypoints.
- Applying a live manifest does not authorize execution; the Monday PLOY daemon uses
  a fail-closed gateway for probe, submit, cancel, replace, and reconciliation.
- Do not copy these files into Monday `rust_hft` deployment configuration without a
  separate architecture, risk, and security review.

For current development and validation, start with
`example.paper.json` and the root `.github/workflows/ploy-ci.yml`.
