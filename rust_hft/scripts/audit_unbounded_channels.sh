#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

audit_doc="docs/reports/UNBOUNDED_CHANNEL_AUDIT.md"
pattern='unbounded_channel|UnboundedSender|UnboundedReceiver|UnboundedReceiverStream'
scope=(
  market-core
  risk-control
  strategy-framework
  apps
  data-pipelines
)

if [[ ! -f "$audit_doc" ]]; then
  echo "missing audit doc: $audit_doc" >&2
  exit 1
fi

missing=0
while IFS= read -r path; do
  [[ -z "$path" ]] && continue
  if ! grep -Fq "\`$path\`" "$audit_doc"; then
    echo "unclassified unbounded channel usage: $path" >&2
    missing=1
  fi
done < <(rg -l "$pattern" "${scope[@]}" -S | sort || true)

if [[ "$missing" -ne 0 ]]; then
  echo "update $audit_doc with a classification before merging" >&2
  exit 1
fi

echo "all current unbounded channel usages are classified in $audit_doc"
