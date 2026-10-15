#!/usr/bin/env bash
# Cache only the pinned license tool, independently of application lockfiles.
set -euo pipefail
version=0.7.0
tool_root=${RUNNER_TEMP:?}/cargo-license-tool
binary=$tool_root/bin/cargo-license
receipt=$tool_root/receipt.json
script=$(cd "$(dirname "$0")" && pwd)/$(basename "$0")

digest() { sha256sum | awk '{print $1}'; }
identity() {
  { printf '%s\n' "$version" "${RUNNER_OS:?}" "${RUNNER_ARCH:?}" "${ImageOS:?}"
    rustc -vV
    cat "$script"
  } | digest
}

case ${1:-} in
  prepare)
    namespace=trusted
    save_allowed=false
    if [[ ${GITHUB_REF:-} == refs/heads/main ]]; then
      case ${GITHUB_EVENT_NAME:-} in push|schedule|workflow_dispatch) save_allowed=true ;; esac
    fi
    case ${LICENSE_CACHE_PROBE:-false} in
      true)
        [[ ${GITHUB_EVENT_NAME:-} == workflow_dispatch ]] || {
          echo 'License cache probes require workflow_dispatch' >&2; exit 2;
        }
        if [[ ${GITHUB_REF:-} != refs/heads/main ]]; then
          [[ ${GITHUB_SHA:-} =~ ^[a-f0-9]{40}$ ]] || exit 2
          namespace=probe-$GITHUB_SHA
          save_allowed=true
        fi
        ;;
      false|'') ;;
      *) echo 'Invalid license cache probe input' >&2; exit 2 ;;
    esac
    printf 'key=cargo-license-v1-%s-%s\npath=%s\nsave_allowed=%s\n' \
      "$namespace" "$(identity)" "$tool_root" "$save_allowed" >>"${GITHUB_OUTPUT:?}"
    ;;
  install)
    began=$SECONDS
    case ${2:-} in
      true) ;;
      false|'')
        # Do not inherit the application's Cargo config, profile or target dir.
        toolchain=$(rustup show active-toolchain | awk '{print $1}')
        rm -rf -- "$tool_root"
        (cd "${RUNNER_TEMP:?}" && \
          env -u CARGO_TARGET_DIR -u CARGO_BUILD_TARGET -u RUSTFLAGS -u CARGO_ENCODED_RUSTFLAGS \
          RUSTUP_TOOLCHAIN="$toolchain" cargo install --locked --version "$version" \
          --root "$tool_root" cargo-license)
        installed=$(cargo install --list --root "$tool_root")
        grep -Fxq "cargo-license v$version:" <<<"$installed"
        jq -n --arg version "$version" --arg identity "$(identity)" \
          --arg sha256 "$(digest <"$binary")" \
          '{schema:"monday.cargo-license-cache.v1",version:$version,identity:$identity,sha256:$sha256}' >"$receipt"
        rm -f "$tool_root/.crates.toml" "$tool_root/.crates2.json"
        ;;
      *) echo 'Invalid cache-hit result' >&2; exit 2 ;;
    esac
    [[ -f $binary && -x $binary && ! -L $binary && -f $receipt && ! -L $receipt ]]
    jq -e --arg version "$version" --arg identity "$(identity)" \
      --arg sha256 "$(digest <"$binary")" \
      '.schema=="monday.cargo-license-cache.v1" and .version==$version and .identity==$identity and .sha256==$sha256' \
      "$receipt" >/dev/null
    "$binary" --help >/dev/null
    printf '%s\n' "$tool_root/bin" >>"${GITHUB_PATH:?}"
    printf 'cargo-license version=%s cache_hit=%s install_seconds=%s sha256=%s\n' \
      "$version" "${2:-false}" "$((SECONDS-began))" "$(digest <"$binary")"
    ;;
  *) echo 'usage: cargo-license-tool.sh prepare|install [cache-hit]' >&2; exit 2 ;;
esac
