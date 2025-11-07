# Configuration Files

All CLI subcommands support YAML configuration files to reduce command-line clutter.

## Available Configs

| Config File | CLI Command | Description |
|-------------|-------------|-------------|
| `qa.yaml` | `python cli.py qa run --config configs/qa.yaml` | Data quality checks (coverage, schema, gaps) |
| `features_build.yaml` | `python cli.py dataset build --config configs/features_build.yaml` | Multi-symbol feature dataset construction |
| `train_fit.yaml` | `python cli.py train fit --config configs/train_fit.yaml` | Supervised DL model training |
| `unsup_train.yaml` | `python cli.py unsup train --config configs/unsup_train.yaml` | Unsupervised TCN-GRU training |
| `backtest.yaml` | `python cli.py backtest dl --config configs/backtest.yaml` | Deep learning backtesting |

## Environment Variables

Sensitive values should use environment variable substitution:

```yaml
host: ${CLICKHOUSE_HOST}
user: ${CLICKHOUSE_USERNAME}
password: ${CLICKHOUSE_PASSWORD}
```

See `.env.example` for required variables.

## Validation

Configs are validated at load time. Missing required fields will cause early failure:

```bash
# Good: all required fields present
python cli.py qa run --config configs/qa.yaml

# Bad: will fail with clear error message
python cli.py qa run --config configs/invalid.yaml
```

## Override Behavior

CLI flags override config values:

```bash
# Config specifies symbol: WLFIUSDT
# CLI flag overrides to BTCUSDT
python cli.py qa run --config configs/qa.yaml --symbol BTCUSDT
```

## Best Practices

1. **Never commit secrets** - Use `${VAR}` syntax for credentials
2. **Document required fields** - Add comments for non-obvious parameters
3. **Provide examples** - These configs serve as templates for new use cases
4. **Version datasets** - Include timestamps in output paths for reproducibility
