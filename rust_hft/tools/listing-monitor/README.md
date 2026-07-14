# Listing Monitor

`listing-monitor-alpha` and `listing-monitor-spot` are the only supported
runtime entrypoints. Both are Rust binaries and fail during startup unless
`FEISHU_WEBHOOK_URL` is present and points to an official Feishu or Lark HTTPS
host.

The ECS workflow injects that value from AWS Secrets Manager. Configure the
GitHub secret `FEISHU_WEBHOOK_SECRET_ARN` with the secret ARN and grant the ECS
task execution role `secretsmanager:GetSecretValue`. Do not put the webhook URL
in a task definition, environment file, image, log, or source code.

Before deploying this version, rotate any webhook that was previously stored in
repository history.

Build from the Rust workspace root:

```bash
docker build -f tools/listing-monitor/Dockerfile -t listing-monitor .
```

Run either monitor with the secret injected by the runtime:

```bash
listing-monitor-alpha
listing-monitor-spot
```
