# Emergency Stop

> **Historical standalone PLOY operations.** Host, systemd, secret, and nested
> workflow commands below are retained as migration evidence only. They do not
> define a current Monday deployment or execution path.

## Option 1: API Stop (Preferred)

Sends a graceful shutdown signal through the application. Cancels open orders
and persists state before exiting.

```bash
curl -X POST http://localhost:8081/api/emergency-stop \
  -H "x-ploy-admin-token: $ADMIN_TOKEN"
```

Use this when:
- The application is responsive.
- You want open orders cancelled cleanly.
- You want state persisted before shutdown.

## Option 2: systemctl Stop

Sends SIGTERM to the process. The application has a 30s graceful shutdown window.

```bash
sudo systemctl stop ployd
```

Use this when:
- The API is unresponsive.
- The application is stuck or deadlocked.
- Option 1 failed or timed out.

## Option 3: Force Kill

Last resort. No graceful shutdown, no state persistence.

```bash
sudo systemctl kill -s SIGKILL ployd
```

Use only when systemctl stop hangs beyond 30s.

## Post-stop Verification

```bash
systemctl is-active ployd         # should say "inactive"
ss -tlnp | grep 8081              # port should be free
journalctl -u ployd -n 50 --no-pager  # check shutdown logs
```

## Resuming

After the emergency is resolved, follow `restart.md` to bring the service back.
