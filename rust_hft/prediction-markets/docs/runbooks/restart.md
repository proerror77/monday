# Safe Restart

> **Historical standalone PLOY operations.** Host, systemd, secret, and nested
> workflow commands below are retained as migration evidence only. They do not
> define a current Monday deployment or execution path.

## When to Use

Use this procedure for planned restarts (config changes, minor updates).
For emergencies, see `emergency-stop.md` instead.

## Procedure

1. Check current state:
   ```bash
   systemctl status ployd
   journalctl -u ployd -n 20 --no-pager
   ```

2. Governance state is auto-persisted to the database (R-07 fix).
   No manual state export is needed before restart.

3. Stop the service:
   ```bash
   sudo systemctl stop ployd
   ```

4. Wait for graceful shutdown (up to 30s). Verify:
   ```bash
   systemctl is-active ployd  # should say "inactive"
   ```

5. Start the service:
   ```bash
   sudo systemctl start ployd
   ```

6. Verify:
   ```bash
   systemctl show ployd -p MemoryMax -p Restart -p OOMPolicy
   journalctl -u ployd -n 30 --no-pager
   /opt/ploy/bin/ployctl system status
   ```

## Post-restart Checks

- Confirm positions are recovered from checkpoint (look for "recovery" in logs).
- Confirm governance policies are loaded (look for "governance" in logs).
- Confirm no duplicate orders were placed during restart window.
