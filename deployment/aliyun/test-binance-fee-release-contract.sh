#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "$0")" && pwd)
producer="$script_dir/../../rust_hft/tools/collector/src/bin/binance-fee-snapshot.rs"
spot_service="$script_dir/binance-fee-snapshot-spot.service"
spot_timer="$script_dir/binance-fee-snapshot-spot.timer"
usdm_service="$script_dir/binance-fee-snapshot-usdm.service"
usdm_timer="$script_dir/binance-fee-snapshot-usdm.timer"
upload_service="$script_dir/binance-fee-upload.service"
upload_timer="$script_dir/binance-fee-upload.timer"
upload_env="$script_dir/binance-fee-upload.env"

grep -Fq 'account_secret_file: PathBuf' "$producer"
if grep -Fq 'HFT_SECRET_BINANCE_ACCOUNT_JSON' "$producer"; then
  exit 1
fi

for service in "$spot_service" "$usdm_service"; do
  grep -Fxq 'LoadCredential=binance-account.json:/etc/monday/credentials/binance-account.json' "$service"
  grep -Fq -- '--account-secret-file %d/binance-account.json' "$service"
  grep -Fxq 'ReadWritePaths=/data/monday/spool/binance-fee' "$service"
  if grep -Eq '^(Environment|EnvironmentFile)=.*(api_key|secret|credential)' "$service"; then
    exit 1
  fi
done
grep -Fq -- '--market spot --symbol BTCUSDT' "$spot_service"
grep -Fq -- '--market usdm --symbol BTCUSDT' "$usdm_service"

for timer in "$spot_timer" "$usdm_timer"; do
  grep -Fxq 'OnUnitActiveSec=60s' "$timer"
  grep -Fxq 'Persistent=true' "$timer"
done
grep -Fxq 'Unit=binance-fee-snapshot-spot.service' "$spot_timer"
grep -Fxq 'Unit=binance-fee-snapshot-usdm.service' "$usdm_timer"

grep -Fxq 'EnvironmentFile=/etc/monday/binance-fee-upload.env' "$upload_service"
grep -Fxq 'ExecStart=/opt/monday/bin/binance-fee-snapshot-upload --output-root /data/monday/spool/binance-fee' "$upload_service"
grep -Fxq 'ReadWritePaths=/data/monday/spool/binance-fee' "$upload_service"
grep -Fxq 'OnUnitActiveSec=60s' "$upload_timer"
grep -Fxq 'Unit=binance-fee-upload.service' "$upload_timer"
grep -Fxq 'OSS_BUCKET=monday-lob-apne1-1045353359' "$upload_env"
grep -Fxq 'OSS_ENDPOINT=oss-ap-northeast-1-internal.aliyuncs.com' "$upload_env"
grep -Fxq 'OSS_REGION=ap-northeast-1' "$upload_env"
grep -Fxq 'ALIYUN_PROFILE=ecs-role' "$upload_env"

printf '%s\n' 'Binance fee release contract tests passed'
