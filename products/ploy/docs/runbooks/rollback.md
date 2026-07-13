# Rollback

## When to Use

- A new release causes crashes, order failures, or unexpected behavior.
- Health checks fail after deployment.
- You need to revert to a known-good binary quickly.

## Procedure

### Option 1: Manual SSH (Current Platform Path)

```bash
ssh root@<host>
sudo systemctl stop ployd
sudo cp /root/ploy/bin/ployd.bak /root/ploy/bin/ployd
sudo chmod +x /root/ploy/bin/ployd
sudo systemctl start ployd
```

If you also rolled `ployctl` forward in the same release, restore
`/root/ploy/bin/ployctl` from the matching backup or release directory before
you resume operator checks.

### Option 2: Legacy Single-Binary Rollback Workflow

The historical "Rollback Production" workflow still applies only to the retired
single-binary `ploy` path. Do not treat it as the default rollback path for the
workspace runtime.

## Post-rollback Checks

1. Verify the service is running:
   ```bash
   systemctl is-active ployd
   systemctl status ployd --no-pager
   ```

2. Check health endpoint:
   ```bash
   /root/ploy/bin/ployctl system status
   ```

3. Verify the correct binary version:
   ```bash
   ls -lh /root/ploy/bin/ployd /root/ploy/bin/ployctl
   ```

4. Monitor logs for the first few minutes:
   ```bash
   journalctl -u ployd -f --no-pager
   ```

5. Confirm no duplicate or stale orders from the failed release.
