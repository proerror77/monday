# Migration to Monday

PLOY was imported into `proerror77/monday` at `products/ploy` on 2026-07-14.

## Source

- Repository: `https://github.com/proerror77/ploy`
- Branch: `main`
- SHA: `8ce4e0f150173a44030294101f4b1371cbdf80bc`
- Import: adapted tracked snapshot produced from `git archive`
- Historical local docs: commit `5de411bbe8889284b47fe9932821af077d2962fc`

The source archive is the provenance baseline, but the Monday tree is intentionally
not byte-identical. See [MIGRATION_ADAPTATIONS.md](MIGRATION_ADAPTATIONS.md) for
the path and SHA-256 manifest covering renamed, changed, replaced, and omitted material.

## Runtime status

- Live trading: disabled
- Monday execution authority: `rust_hft`
- PLOY production gateway: rejects probe, submit, cancel, replace, and reconcile
- Standard runner `full` feature: does not include legacy `live-execution`
- Polymarket and Predict account tools: planning and read checks remain available;
  approval, execution, redemption, and reconciliation writes reject unconditionally
- Write-capable standalone `ploy-openclaw`: relocated under the historical archive
- Legacy PLOY deploy workflows: retained only as nested, inactive source material
- Legacy secrets/environments: not copied

## Former repository configuration inventory

Secret names recorded before archive:

`ALIYUN_ECS_ACCESS_KEY_ID`, `ALIYUN_ECS_ACCESS_KEY_SECRET`, `ALIYUN_ECS_HOST`, `ALIYUN_ECS_SSH_KEY`, `ALIYUN_ECS_USER`, `ALIYUN_OSS_ACCESS_KEY_ID`, `ALIYUN_OSS_ACCESS_KEY_SECRET`, `AWS_ACCESS_KEY_ID`, `AWS_EC2_HOST`, `AWS_EC2_PRIVATE_KEY`, `AWS_SECRET_ACCESS_KEY`, `FEISHU_WEBHOOK_URL`, `PLOY_DB_URL`, `PLOY_RESEARCH_DATABASE_URL`, `PLOY_TRADE_1_HOST`, `PLOY_TRADE_1_SSH_KEY`, `POLYMARKET_FUNDER`, `POLYMARKET_PRIVATE_KEY`, `TANGO_1_1_HOST`, `TANGO_SSH_KEY`.

Variable names: `ALIYUN_ECS_INSTANCE_ID`, `ALIYUN_ECS_INSTANCE_NAME`, `ALIYUN_PLOY_ROOT`, `ALIYUN_REGION`.

Environment names: `ack`, `ploy-ci-1`, `ploy-trade-1`, `ploy-trade-1-build-only`, `ploy-trade-live`, `production`, `tango-1-1`, `tango-1-1-build-only`.

Values were not readable through GitHub and were not recreated. Any future deployment must recover them from the original secure source and re-establish Monday-native approvals rather than copying assumptions from the archived repository.
