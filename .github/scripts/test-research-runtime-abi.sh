#!/usr/bin/env bash
set -euo pipefail
script_dir=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin" "$work/release"
for binary in hft-backtest alpha-harness lob-pit-materializer binance-market-tape-slicer binance-replay-parquet-materializer clickhouse-analytics-materializer monday-prediction-research monday-prediction-evaluator monday-prediction-snapshot; do
  printf 'ELF fixture\n' > "$work/release/$binary"
  chmod 0755 "$work/release/$binary"
done
cat > "$work/bin/readelf" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  --file-header) printf 'Class: ELF64\nMachine: %s\n' "${TEST_ELF_MACHINE:-Advanced Micro Devices X86-64}" ;;
  --version-info) printf 'Name: %s\n' "${TEST_GLIBC_VERSION:-GLIBC_2.36}" ;;
  *) exit 2 ;;
esac
MOCK
chmod +x "$work/bin/readelf"
PATH="$work/bin:$PATH" bash "$script_dir/verify-research-runtime-abi.sh" "$work/release"
for rejected in GLIBC_2.39 GLIBC_3.0 GLIBC_PRIVATE; do
  if TEST_GLIBC_VERSION="$rejected" PATH="$work/bin:$PATH" bash "$script_dir/verify-research-runtime-abi.sh" "$work/release" > "$work/rejected" 2>&1; then
    echo "unsupported runtime ABI accepted: $rejected" >&2; exit 1
  fi
  grep -Fq 'research runtime ABI rejected:' "$work/rejected"
done
if TEST_ELF_MACHINE=AArch64 PATH="$work/bin:$PATH" bash "$script_dir/verify-research-runtime-abi.sh" "$work/release"; then
  echo 'wrong architecture accepted' >&2; exit 1
fi
echo 'PASS: compatible ABI passes; actual GLIBC_2.39 regression, private symbols and wrong architecture fail closed'
