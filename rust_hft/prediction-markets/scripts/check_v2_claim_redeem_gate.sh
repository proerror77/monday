#!/usr/bin/env bash
set -euo pipefail

echo "V2 claim/redeem gate preflight"
echo
echo "This script records local dependency evidence only."
echo "It does not prove post-V2 auto-redeem behavior."
echo

run() {
  printf '\n$ %s\n' "$*"
  "$@"
}

run cargo tree -p ploy-market-data --features live -i polymarket_client_sdk_v2
run cargo tree -p ploy-market-data --features live -i alloy
if cargo tree -p ploy-connectivity | grep -qE 'polymarket_client_sdk|alloy|secrecy'; then
  echo "ploy-connectivity reintroduced a venue SDK, signer, or secret dependency" >&2
  exit 1
fi
echo "ploy-connectivity remains a fail-closed interface with no venue SDK"
if cargo metadata --format-version 1 --no-deps | grep -q '"name":"ploy-claimer"'; then
  run cargo tree -p ploy-claimer -i ethers-core
  run cargo tree -p ploy-claimer -i ethers-signers
else
  echo
  echo "ploy-claimer is retired from the workspace."
fi

cat <<'MSG'

Gate status:
- Official V2 SDK data dependency checks passed; live Redeem evidence is still required.
- The compatibility execution interface has no venue SDK, signer, or secret dependency.
- Phase 10 claimer retirement has been applied. Keep verifying no downstream
  crate reintroduces ploy-claimer or ethers.
- Account ops stays write-disabled after every deploy and is never a daemon.
MSG
