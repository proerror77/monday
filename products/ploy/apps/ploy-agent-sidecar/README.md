# PLOY Rust agent sidecar

`ploy-agent-sidecar` is the sole consumer for requests accepted by
`POST /api/agent/runs`. `new-ployd` appends those typed requests to
`run/sidecar/agent-run-requests.jsonl`; this process claims that file with an
atomic rename, records terminal attempts, and recovers an unfinished claim after
a restart. Producer/consumer file locks close the open-inode rename race, and a
process-lifetime lease rejects a second worker for the same queue.

The worker is research-only. It reads PLOY control-plane snapshots, invokes the
Codex CLI as a prompt-only child with user configuration, MCP, shell, browser,
plugins, and file tools disabled, or the xAI Grok HTTPS API. The Codex process
receives an explicit environment allowlist rather than PLOY, database, xAI, or
cloud credentials. It has no order, intent, deployment-control, or file-edit
tool path.

This repository does not currently bundle the Rust evidence adapters named by
`requires_data_audit`, `requires_grok_decision`, `requires_executable_replay`,
`requires_full_depth_clob`, or `requires_runtime_parity`. Such requests are
recorded as terminal, fail-closed runs before a model call. Restoring those
capabilities requires a release-pinned Rust parent adapter; user-global MCP
configuration is never loaded.

## Run

From `products/ploy`:

```sh
export PLOY_SIDECAR_AUTH_TOKEN='<shared read-only token>'
cargo run -p new-ployd
cargo run -p ploy-agent-sidecar
```

Both processes must receive the same non-empty `PLOY_SIDECAR_AUTH_TOKEN`. The
worker performs strict live reads of system status, deployments, and trading
state at startup and before every model run. Missing credentials, HTTP 401, or
an unavailable control plane fail closed before any model invocation; the worker
never silently substitutes an on-disk snapshot.

There is intentionally no approved deployment package or systemd unit for this
worker yet. The nested PLOY deployment workflows are historical material, not
Monday deployment entrypoints. For local validation, configure the shared token
and a trusted `CODEX_CLI_BIN` (or Grok credentials) and run the binary directly.
A separately reviewed Monday deployment task must define secret injection,
process supervision, and evidence-adapter packaging before enabling it on a host.

The worker lease enforces exactly one sidecar process for each file-backed queue
directory. JSONL recovery covers process crashes and uses synced appends, but
the two-file request/run journal has no client idempotency key and is not a
database transaction across a host power loss. A malformed final fragment with
no newline (the identifiable interrupted-append signature) is moved to a
private `*.corrupt-tail-*` quarantine file and the valid prefix continues.
Malformed complete lines remain fail-closed and require operator recovery.

## Environment

| Variable | Default | Purpose |
|---|---|---|
| `PLOY_API_URL` | `http://localhost:8081` | Loopback-only PLOY control-plane address; remote HTTP is rejected |
| `PLOY_SIDECAR_AUTH_TOKEN` | required | Dedicated read-only sidecar credential shared with `new-ployd`; admin/operator credentials are ignored |
| `PLOY_RUNTIME_ROOT` | `run/platform` | Root used to derive the sibling queue and run-record directory; never used as a live-context fallback |
| `PLOY_AGENT_RUNS_FILE` | `<PLOY_RUNTIME_ROOT parent>/sidecar/agent-runs.jsonl` | Shared run history consumed by `GET /api/agent/runs` |
| `PLOY_AGENT_RUN_REQUESTS_FILE` | sibling `agent-run-requests.jsonl` | Optional assertion only; startup rejects a value that differs from the daemon-derived path |
| `PLOY_AGENT_RUN_IN_PROGRESS_FILE` | sibling `agent-run-requests.in-progress.jsonl` | Crash-recovery claim |
| `PLOY_HARNESS_CONTEXT_FILE` / `PLOY_HARNESS_EVENTS_FILE` | siblings of the run log | Optional assertions only; startup rejects paths the daemon API would not read |
| `SIDECAR_AGENT_ENGINE` | `codex` | `codex` or `grok` |
| `SIDECAR_POLL_INTERVAL_SECS` | `300` | Non-overlapping poll interval |
| `SIDECAR_AGENT_RUN_MAX_RETRIES` | `1` | Retry attempts after a contract requests retry |
| `SIDECAR_MAX_TURNS` | `30` | Optional lower cap; cannot raise the hard cap |
| `SIDECAR_MAX_BUDGET_USD` | `1` | Optional lower admission cap; cannot raise the hard cap and is not provider billing enforcement |
| `SIDECAR_SCAN_ENABLED` | `false` | Reserved; `true` is rejected until a Rust sports/market evidence adapter is bundled |
| `SIDECAR_DRY_RUN` | `true` | Logged safety contract; mutations remain unavailable in either value |
| `CODEX_CLI_BIN` | `codex` | Trusted, release-pinned Codex executable, invoked directly without a shell |
| `CODEX_CLI_MODEL` | Codex default | Optional model override |
| `CODEX_CLI_SANDBOX` | `read-only` | Optional assertion only; startup rejects any value other than `read-only` |
| `CODEX_CLI_TIMEOUT_SECS` | `600` | Child-process deadline |
| `XAI_API_KEY` / `GROK_API_KEY` | unset | Grok credential |
| `XAI_MODEL` | `grok-4.5` | Grok model |
| `XAI_CHAT_COMPLETIONS_URL` | `https://api.x.ai/v1/chat/completions` | HTTPS-only Grok endpoint |

## Checks

```sh
cargo test -p ploy-agent-sidecar
cargo clippy -p ploy-agent-sidecar --all-targets --no-deps -- -D warnings
```

The API and worker share byte/enum admission limits before model execution. The
tests cover those caps, evaluator gates, prompt-only Codex isolation, awaited
polling, producer/rename coordination, single-worker enforcement, queue crash
recovery, and retry deduplication.
