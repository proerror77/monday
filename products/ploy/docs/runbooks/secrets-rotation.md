# Secrets Rotation

## Pre-flight

Before rotating any secret, check for leaked values in systemd unit files:

```bash
grep -r POLYMARKET /etc/systemd/system/
grep -r GROK_API /etc/systemd/system/
```

If found, remove the plaintext value and redeploy via `release-platform.yml`.

## Rotate POLYMARKET_PRIVATE_KEY

1. Generate or obtain the new private key.
2. Update the GitHub secret: Settings > Secrets > `POLYMARKET_PRIVATE_KEY`.
3. Trigger `release-platform.yml` (workflow_dispatch) to redeploy.
4. On host, verify the service started: `systemctl status ployd`.
5. Confirm the old key is not on disk: `grep -r POLYMARKET /etc/systemd/system/`.

## Rotate GROK_API_KEY

1. Generate a new key in the Grok dashboard.
2. Update the GitHub secret: Settings > Secrets > `GROK_API_KEY`.
3. Trigger `release-platform.yml` to redeploy.
4. Verify: `systemctl status ployd` and check logs for Grok API calls.

## Rotate SSH Keys

1. Generate a new Ed25519 key pair: `ssh-keygen -t ed25519`.
2. Add the new public key to `~/.ssh/authorized_keys` on the target host.
3. Update the GitHub secret: `ALIYUN_ECS_SSH_KEY` (or `EC2_SSH_KEY`).
4. Test connectivity: trigger a manual workflow that SSHes into the host.
5. Remove the old public key from `authorized_keys` on the host.
