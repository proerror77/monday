# PLOY data jobs inventory inside Monday

This inventory describes current ownership after the PLOY migration. It supersedes
the standalone host/workflow inventory at source SHA
`8ce4e0f150173a44030294101f4b1371cbdf80bc`.

| Surface | Current status | Authority |
| --- | --- | --- |
| `crates/ploy-market-data` | maintained PLOY market-data/product code | PLOY product workspace |
| `crates/ploy-feed-loaders` | maintained historical loaders | PLOY research/backtest |
| `scripts/*` collectors and repairs | compatibility or offline tools | explicit task review only |
| `migrations/*` | retained PLOY schema history | database-backed CI and reviewed migrations |
| root `.github/workflows/ploy-ci.yml` | active PLOY validation | Monday repository |
| nested `products/ploy/.github/workflows/*` | historical, inactive | no deployment authority |
| former `/opt/ploy` and Tango/trade hosts | historical topology | no Monday authority |

## Guardrails

- Production data acquisition and host deployment stay under Monday's governed
  collector/runtime ownership.
- Do not dispatch nested workflows or revive former host mutation from this inventory.
- Do not treat compatibility Python collectors as a production source of truth without
  explicit parity, freshness, and ownership evidence.
- Research artifacts do not authorize live execution or promotion.

Detailed standalone classifications are preserved by the source SHA and the verified
pre-archive Git bundle recorded in `MIGRATION_ADAPTATIONS.md`.
