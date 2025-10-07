#!/usr/bin/env bash
set -euo pipefail

# Remove duplicate/unnecessary Grafana dashboards safely
# Usage:
#   GRAFANA_URL=http://localhost:3000 GRAFANA_USER=admin GRAFANA_PASSWORD=admin \
#   bash scripts/grafana_cleanup.sh

GRAFANA_URL="${GRAFANA_URL:-http://localhost:3000}"
GRAFANA_USER="${GRAFANA_USER:-admin}"
GRAFANA_PASSWORD="${GRAFANA_PASSWORD:-admin}"

if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required. Please install jq first." >&2
  exit 1
fi

echo "Fetching dashboards from: $GRAFANA_URL ..." >&2
dash_json=$(curl -s -u "$GRAFANA_USER:$GRAFANA_PASSWORD" \
  "$GRAFANA_URL/api/search?type=dash-db&query=&limit=5000")

if [[ -z "$dash_json" || "$dash_json" == "null" ]]; then
  echo "Failed to fetch dashboards (empty response)." >&2
  exit 1
fi

# Titles to keep (desired final set)
read -r -d '' KEEP_TITLES << 'EOF' || true
HFT System Overview - Professional
HFT Data Collection Dashboard - Fixed
LOB Depth Overview (Fixed)
EOF

# Convert keep list to jq-compatible array
keep_array=$(printf '%s\n' "$KEEP_TITLES" | jq -R . | jq -s .)

# Heuristic patterns to delete (common duplicates)
read -r -d '' DELETE_PATTERNS << 'EOF' || true
Copy of 
(old)
(legacy)
Deprecated
Test
tmp
演示
示例
樣板
模板
EOF

pattern_array=$(printf '%s\n' "$DELETE_PATTERNS" | jq -R . | jq -s .)

echo "Evaluating dashboards to delete..." >&2

to_delete=$(jq -r --argjson keep "$keep_array" --argjson pats "$pattern_array" '
  def keep_has($t): any($keep[]; . == $t);
  def match_any($t): any($pats[]; $t | test(. ; "i"));
  [ .[]
    | select(.type == "dash-db")
    | . as $d | $d.title as $t
    | select( (keep_has($t) | not)
             and ( match_any($t)
                   or ($t | test("^Copy of |^HFT System Overview$|^HFT Data Collection Dashboard$|^HFT Redis Live$|^LOB Depth Overview$"; "i")) ) )
    | {uid: $d.uid, title: $t, url: $d.url}
  ] | .[]
' <<< "$dash_json")

if [[ -z "$to_delete" ]]; then
  echo "No dashboards matched deletion criteria. Nothing to do." >&2
  exit 0
fi

echo "The following dashboards will be deleted:" >&2
echo "$to_delete" | jq -r '.uid + "\t" + .title + "\t" + .url'

read -r -p "Proceed with deletion? (y/N): " ans
if [[ ! "$ans" =~ ^[Yy]$ ]]; then
  echo "Aborted by user." >&2
  exit 0
fi

echo "$to_delete" | jq -r '.uid' | while read -r uid; do
  echo "Deleting UID=$uid ..." >&2
  curl -s -o /dev/null -w "%{http_code}\n" -u "$GRAFANA_USER:$GRAFANA_PASSWORD" \
    -X DELETE "$GRAFANA_URL/api/dashboards/uid/$uid"
done

echo "Cleanup complete." >&2
