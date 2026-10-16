#!/usr/bin/env bash
# Reuse only the original PostgreSQL/rustls CLI profile, not default-feature binaries.
set -euo pipefail
version=0.8.6
[[ ${SQLX_CLI_VERSION:-$version} == "$version" ]] || { echo 'SQLx recipe version mismatch' >&2; exit 2; }
tool_root=${RUNNER_TEMP:?}/sqlx-cli-tool
receipt=$tool_root/receipt.json
script=$(cd "$(dirname "$0")" && pwd)/$(basename "$0")
digest() { sha256sum | awk '{print $1}'; }
identity() {
  { printf '%s\n' "$version" 'no-default-features;postgres,rustls;locked' \
      "${RUNNER_OS:?}" "${RUNNER_ARCH:?}" "${ImageOS:?}"
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
    case ${SQLX_CACHE_PROBE:-false} in
      true)
        [[ ${GITHUB_EVENT_NAME:-} == workflow_dispatch ]] || exit 2
        if [[ ${GITHUB_REF:-} != refs/heads/main ]]; then
          [[ ${GITHUB_SHA:-} =~ ^[a-f0-9]{40}$ ]] || exit 2
          namespace=probe-$GITHUB_SHA
          save_allowed=true
        fi
        ;;
      false|'') ;;
      *) exit 2 ;;
    esac
    printf 'key=sqlx-cli-v1-%s-%s\npath=%s\nsave_allowed=%s\n' \
      "$namespace" "$(identity)" "$tool_root" "$save_allowed" >>"${GITHUB_OUTPUT:?}"
    ;;
  install)
    began=$SECONDS
    case ${2:-} in
      true) ;;
      false|'')
        toolchain=$(rustup show active-toolchain | awk '{print $1}')
        rm -rf -- "$tool_root"
        (cd "${RUNNER_TEMP:?}" && \
          env -u CARGO_TARGET_DIR -u CARGO_BUILD_TARGET -u RUSTFLAGS -u CARGO_ENCODED_RUSTFLAGS \
          RUSTUP_TOOLCHAIN="$toolchain" cargo install sqlx-cli --locked --version "$version" \
          --no-default-features --features rustls,postgres --root "$tool_root")
        # Cargo records the actual feature selection, not just the package version.
        jq -e --arg version "$version" '
          .installs | to_entries | map(select(.key | startswith("sqlx-cli "+$version+" "))) |
          length==1 and (.[0].value | .no_default_features==true and .all_features==false and
          (.features|sort)==["postgres","rustls"] and (.bins|sort)==["cargo-sqlx","sqlx"])
        ' "$tool_root/.crates2.json" >/dev/null
        jq -n --arg version "$version" --arg identity "$(identity)" \
          --arg sqlx "$(digest <"$tool_root/bin/sqlx")" \
          --arg cargo_sqlx "$(digest <"$tool_root/bin/cargo-sqlx")" \
          '{schema:"monday.sqlx-cli-cache.v1",version:$version,features:["postgres","rustls"],
            no_default_features:true,identity:$identity,sqlx:$sqlx,cargo_sqlx:$cargo_sqlx}' >"$receipt"
        rm -f "$tool_root/.crates.toml" "$tool_root/.crates2.json"
        ;;
      *) exit 2 ;;
    esac
    for name in sqlx cargo-sqlx; do
      [[ -f $tool_root/bin/$name && -x $tool_root/bin/$name && ! -L $tool_root/bin/$name ]]
    done
    [[ -f $receipt && ! -L $receipt ]]
    jq -e --arg version "$version" --arg identity "$(identity)" \
      --arg sqlx "$(digest <"$tool_root/bin/sqlx")" --arg cargo_sqlx "$(digest <"$tool_root/bin/cargo-sqlx")" '
      .schema=="monday.sqlx-cli-cache.v1" and .version==$version and .identity==$identity and
      .features==["postgres","rustls"] and .no_default_features==true and .sqlx==$sqlx and .cargo_sqlx==$cargo_sqlx
    ' "$receipt" >/dev/null
    [[ $("$tool_root/bin/sqlx" --version) == "sqlx-cli $version" ]]
    printf '%s\n' "$tool_root/bin" >>"${GITHUB_PATH:?}"
    printf 'sqlx-cli version=%s features=postgres,rustls cache_hit=%s install_seconds=%s sha256=%s\n' \
      "$version" "${2:-false}" "$((SECONDS-began))" "$(digest <"$tool_root/bin/sqlx")"
    ;;
  *) echo 'usage: sqlx-cli-tool.sh prepare|install [cache-hit]' >&2; exit 2 ;;
esac
