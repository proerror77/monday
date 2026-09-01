#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(git -C "$script_dir" rev-parse --show-toplevel)
producer="$repo_root/rust_hft/tools/collector/src/bin/binance-usdm-account-archiver.rs"
cargo_manifest="$repo_root/rust_hft/tools/collector/Cargo.toml"
dockerfile="$repo_root/rust_hft/deployment/docker/Dockerfile.binance-lob-archiver"
workflow="$repo_root/.github/workflows/acr-publish.yml"
service="$script_dir/binance-usdm-account-archiver.service"

grep -Fq 'account_secret_file: Option<PathBuf>' "$producer"
grep -Fq 'fn read_account_secret(path: &Path)' "$producer"
grep -Fq '"runtime_account_id": self.runtime_account_id' "$producer"
grep -Fq '"account_fingerprint": self.account_fingerprint' "$producer"
grep -Fq 'CAPTURE_GAP' "$producer"
grep -Fq 'verify_spool_ready(' "$producer"
if grep -Fq 'BINANCE_API_KEY' "$producer" || grep -Fq 'BINANCE_API_SECRET' "$producer"; then
  exit 1
fi
grep -Fqx 'name = "binance-usdm-account-archiver"' "$cargo_manifest"
grep -Fq -- '--bin binance-usdm-account-archiver' "$dockerfile"
grep -Fq '/usr/local/bin/binance-usdm-account-archiver' "$dockerfile"
grep -Fq 'tee binance-usdm-account-archiver.sha256' "$workflow"
grep -Fq 'binance-usdm-account-production-control-assets.sha256' "$workflow"
grep -Fq 'binance-usdm-account-production-control.tar.gz' "$workflow"
grep -Fq 'monday.binance_usdm_account_release.v1' "$workflow"
grep -Fq 'binance-usdm-account-release.json.sha256' "$workflow"

grep -Fxq 'LoadCredential=binance-account.json:/etc/monday/credentials/binance-account.json' "$service"
grep -Fq -- '--account-secret-file %d/binance-account.json' "$service"
grep -Fq -- '--max-spool-bytes 53687091200' "$service"
grep -Fq -- '--min-free-bytes 21474836480' "$service"
grep -Fxq 'ReadWritePaths=/data/monday/spool/binance-usdm-account' "$service"
grep -Fxq 'UMask=0077' "$service"
grep -Fxq 'NoNewPrivileges=true' "$service"
if grep -Eqi '^(Environment|EnvironmentFile)=.*(api_key|secret|credential)' "$service"; then
  exit 1
fi

printf '%s\n' 'Binance USD-M account release contract tests passed'
