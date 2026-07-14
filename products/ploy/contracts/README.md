# Operator Contracts

`ploy-operator-contracts` is the source of truth for control-plane DTOs.

- Rust schema snapshots live in `contracts/schemas/*.schema.json`.
- TypeScript contract types are generated from those snapshots into:
  - `ploy-frontend/src/types/operator-contracts.ts`
  - `ploy-sidecar/src/contracts/operator-contracts.ts`

Regenerate after changing Rust DTOs:

```sh
cargo run -p ploy-operator-contracts --example export_schemas
node scripts/export_operator_contract_types.mjs
```

Check for stale contracts:

```sh
cargo run -p ploy-operator-contracts --example export_schemas -- --check
node scripts/export_operator_contract_types.mjs --check
```
