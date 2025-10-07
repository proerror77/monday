#!/usr/bin/env bash
set -euo pipefail

# Cleanup large/binary artifacts and legacy chunks from workspace to slim down collector tree
# Usage:
#   DRY_RUN=1 ./scripts/cleanup_collector_workspace.sh   # preview only (default)
#   RUN=1     ./scripts/cleanup_collector_workspace.sh   # actually delete

ROOT_DIR=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT_DIR"

DRY=${DRY_RUN:-1}

patterns=(
  "apps/collector/hft-collector-chunk*"
  "apps/collector/hft-collector-standard.tar.gz.part.*"
  "apps/collector/*.tar.gz"
  "apps/collector/*.tar"
  "apps/collector/collector-*.tar.gz"
  "hft-collector-*.tar.gz"
  "collector-*.tar.gz"
)

echo "[info] Scanning for removable artifacts..."
to_delete=()
total_size=0
for pat in "${patterns[@]}"; do
  while IFS= read -r -d '' f; do
    # skip if file does not exist
    [ -e "$f" ] || continue
    to_delete+=("$f")
  done < <(find . -path "./.git" -prune -o -type f -name "$(basename "$pat")" -print0 2>/dev/null || true)
done

if [ ${#to_delete[@]} -eq 0 ]; then
  echo "[info] Nothing to delete."
  exit 0
fi

printf "[info] Found %d files. Listing top 20 by size...\n" "${#to_delete[@]}"
printf "%s\n" "${to_delete[@]}" | xargs -I{} du -h {} 2>/dev/null | sort -hr | head -20

if [ "${RUN:-0}" != "1" ]; then
  echo "[dry-run] Set RUN=1 to actually delete."
  exit 0
fi

echo "[info] Deleting files..."
printf "%s\n" "${to_delete[@]}" | xargs -I{} sh -c 'sz=$(stat -f %z "{}" 2>/dev/null || stat -c %s "{}" 2>/dev/null || echo 0); echo "{}"; rm -f "{}"; echo "$sz"' >/tmp/_del_list_and_sizes.txt

freed=$(awk 'NR%2==0{sum+=$1} END{print sum+0}' /tmp/_del_list_and_sizes.txt)
echo "[done] Deleted ${#to_delete[@]} files; freed $freed bytes (~$(awk -v b=$freed 'BEGIN{printf "%.2f", b/1024/1024/1024}') GiB)."

