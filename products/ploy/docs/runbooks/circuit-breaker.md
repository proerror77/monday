# Circuit Breaker

## States

- **Closed**: Normal operation. Failures are counted.
- **Open**: Tripped after threshold failures. All requests are rejected.
- **HalfOpen**: After cooldown, a limited number of requests are allowed through.
  Successes transition back to Closed; failures re-open the breaker.

## Inspect Current State

The current workspace control plane no longer exposes the legacy
`/api/sidecar/risk` view. Use the default operator surfaces instead:

```bash
ployctl system status
ployctl trading status
curl -s http://localhost:8081/api/system/status | jq
journalctl -u ployd --no-pager | grep -i "circuit\\|degraded\\|recovering" | tail -20
```

## Manual Reset

If the circuit breaker is stuck in Open state after the underlying issue is resolved:

1. Verify the root cause is fixed (e.g., exchange API is back up).
2. Restart the service — the breaker resets to Closed on startup:
   ```bash
   sudo systemctl restart ployd
   ```
3. Monitor logs for the first few minutes to confirm requests succeed.

## Tuning

The legacy coordinator bootstrap knobs are archived with the retired single-binary
runtime. In the current workspace, treat repeated venue failures as a platform
health issue first:

- watch for `degraded` / `recovering` in `ployctl system status`
- inspect the affected deployment in `ployctl trading inspect <deployment-id>`
- review `journalctl -u ployd` before changing any runtime thresholds

## Validation

After reset, confirm:
```bash
journalctl -u ployd -n 30 --no-pager | grep -i "circuit"
ployctl system status
ployctl trading status
```

Expected: the platform is back to `running` or `recovering`, and new requests
are flowing through the canonical control plane again.
