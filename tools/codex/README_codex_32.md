# Codex 32-subagent setup and test

## Run

### macOS / Linux

```bash
python3 tools/codex/codex_subagents_32.py --test
```

### Windows PowerShell

```powershell
py tools/codex/codex_subagents_32.py --test
```

The script:

1. Backs up the existing Codex config.
2. Sets the official global subagent concurrency field to `32`.
3. Checks that Codex can load the configuration.
4. Starts 32 workers and calculates their measured peak overlap.

## Pass condition

```text
RESULT: started=32 completed=32 peak_concurrent=32
TEST PASS: 32 workers overlapped concurrently.
```

A lower `peak_concurrent` means the configuration was accepted, but the current Codex runtime, orchestration layer, account limit, or tool-call scheduler did not execute all 32 simultaneously.

Fully restart Codex App after changing the configuration.
