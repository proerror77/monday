# Plugin Registry

`config/plugins/` is the file-backed registry root for strategy plugin definitions.

Task 1 keeps this registry intentionally small:

- one TOML file per plugin definition
- no database persistence
- no runtime deployment state here yet

Each plugin definition currently supports:

```toml
plugin_id = "crypto.momentum.v1"
kind = "composable_crypto"
version = "v1"
domain = "crypto"
```

Supported `kind` values:

- `composable_crypto`
- `registered_strategy`

This directory is the steady-state source for plugin discovery. Runtime projection,
deployment state, and account binding will be layered on in later tasks.
