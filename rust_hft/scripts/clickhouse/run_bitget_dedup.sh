#!/usr/bin/env bash
set -euo pipefail

# 使用 ClickHouse HTTP 端點執行去重腳本
# 需要環境變數：CLICKHOUSE_URL, CLICKHOUSE_USER, CLICKHOUSE_PASSWORD (可選), CLICKHOUSE_DATABASE

CH_URL=${CLICKHOUSE_URL:-"https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443"}
CH_USER=${CLICKHOUSE_USER:-"default"}
CH_PASS=${CLICKHOUSE_PASSWORD:-""}
CH_DB=${CLICKHOUSE_DATABASE:-"hft_db"}

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &> /dev/null && pwd)
SQL_FILE="$SCRIPT_DIR/bitget_dedup.sql"

if [[ ! -f "$SQL_FILE" ]]; then
  echo "SQL 文件不存在: $SQL_FILE" >&2
  exit 1
fi

echo "執行 Bitget 去重 SQL..."

# 將 SQL 文件拆分為語句進行推送（按分號分隔）
awk 'BEGIN{RS=";"; ORS="\0"} {gsub(/^\s+|\s+$/,"",$0); if(length($0)>1) print $0}' "$SQL_FILE" | \
while IFS= read -r -d '' stmt; do
  # 跳過註釋與空語句
  [[ "$stmt" =~ ^-- ]] && continue
  [[ -z "${stmt// /}" ]] && continue

  # 追加分號（語句結尾）
  stmt="$stmt;"
  if [[ -n "$CH_PASS" ]]; then
    curl -sS -u "$CH_USER:$CH_PASS" -H 'Content-Type: text/plain; charset=UTF-8' \
      --data-binary @- "$CH_URL/?database=$CH_DB" <<< "$stmt" || exit 1
  else
    curl -sS -u "$CH_USER" -H 'Content-Type: text/plain; charset=UTF-8' \
      --data-binary @- "$CH_URL/?database=$CH_DB" <<< "$stmt" || exit 1
  fi
done

echo "✅ 去重 SQL 執行完成（如需切換表名，請手動解除腳本第 4 步註釋並重跑）。"
