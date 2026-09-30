#!/usr/bin/env bash
# shellcheck disable=SC2034 # Active immutable validator functions consume these globals.
# Read-only entrypoint with its own monitoring budget. Reuse the active
# controller's custody validator; never dispatch isolate/drain/retain/resume.
set -Eeuo pipefail
export LC_ALL=C
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
[[ $EUID == 0 && $# == 2 && $1 == check-retained && $2 == spot ]] || exit 2
[[ -z $(compgen -v MONDAY_ || true) ]] || { printf 'monitor reader refuses control overrides\n' >&2; exit 1; }
controller_root=/opt/monday/releases/binance-lob-controller
controller=$(readlink -f -- "$controller_root/active")
controller_sha=${controller##*/}
[[ $controller_sha =~ ^[a-f0-9]{64}$ && $controller == "$controller_root/$controller_sha" ]]
[[ $(sha256sum "$controller/release.json" | awk '{print $1}') == "$controller_sha" ]]
validator="$controller/deployment/host-rust-lob-recovery-queue.sh"
[[ -f $validator && ! -L $validator && $(stat -c %u "$validator") == 0 ]]
# The controller binds every loaded validator byte before any function executes.
(cd "$controller"; sha256sum --check --strict deployment.sha256 >/dev/null)
# shellcheck disable=SC1090
source "$validator"
configure_paths /
MARKET=spot
canonical_paths_safe
market_paths
EXECUTING_RECOVERY_PROGRAM=$(readlink -f -- "$INSTALLED_RECOVERY")
[[ $EXECUTING_RECOVERY_PROGRAM == "$validator" ]]
secure_release_identity
active_recovery_program_matches "$EXECUTING_RECOVERY_PROGRAM"
RETENTION_DEADLINE=$((SECONDS + 300))
check_retained_market
