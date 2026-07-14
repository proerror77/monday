# Operator Terminal Runbook

## Purpose

Operator terminal v1 gives `ployctl` and `ploytui` a control-plane-backed
operations surface.

It is intentionally limited to runtime ops:

- pause
- resume
- force close
- claim check
- claim run

It does not introduce a direct live order path.

## Startup

Start the workspace daemon first:

```bash
export PLOY_API_ADMIN_TOKEN=change-me
cargo run -p new-ployd
```

Then use either the scripted operator client or the terminal console:

```bash
export PLOY_API_ADMIN_TOKEN=change-me
cargo run -p ployctl -- system status
cargo run -p ployctl -- deployments list
cargo run -p ploytui
```

Defaults:

- API base URL: `http://127.0.0.1:${PLOY_API_PORT:-8081}`
- admin header: `x-ploy-admin-token`
- fallback token variable for older operator clients: `PLOY_ADMIN_TOKEN`

## Operator Flow

In `ployctl` or `ploytui`:

1. Refresh the operator snapshot.
2. Inspect system, deployment, and trading status.
3. Pause, resume, or stop deployments through `ployctl deployments ...`.
4. Use claim operations only when the build and account capability explicitly
   support them.

All actions are dispatched through the API control plane.

## Safety Notes

- Missing admin token fails closed.
- claim operations only succeed when the build includes the account capability.
- If the runtime has no active deployment/control-plane owner, mutating
  operations fail closed.
- `ployctl` and `ploytui` are operator clients, not sources of truth; refresh
  from the API when in doubt.
