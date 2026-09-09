#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
installer="$repo_root/.github/workflows/install-ubuntu-packages.sh"
test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT

fake_bin="$test_root/bin"
mkdir -p "$fake_bin"
printf 'test keyring\n' > "$test_root/keyring.gpg"

cat > "$fake_bin/sudo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "rm" ]]; then
  shift
  printf '%s\n' "rm $*" >> "${APT_CALLS:?}"
  /bin/rm "$@"
  exit $?
fi
if [[ "${1:-}" != "apt-get" ]]; then
  echo "unexpected sudo command: $*" >&2
  exit 90
fi
shift
printf '%s\n' "$*" >> "${APT_CALLS:?}"
source_list=""
for argument in "$@"; do
  case "$argument" in
    Dir::Etc::sourcelist=*) source_list="${argument#*=}" ;;
    Dir::Cache::archives=*) archive_dir="${argument#*=}" ;;
  esac
done
if [[ -n "${source_list:-}" && -n "${APT_WORK_DIR:-}" ]]; then
  dirname "$source_list" > "${APT_WORK_DIR:?}"
fi
if [[ " ${*} " == *" update "* ]]; then
  cp "$source_list" "${APT_SOURCES:?}"
  if [[ "${FAIL_UPDATE:-0}" == 1 ]]; then
    exit 41
  fi
  exit 0
fi
if [[ " ${*} " == *" install "* ]]; then
  printf '%s\n' "$*" > "${APT_INSTALL:?}"
  if [[ "${FAIL_INSTALL:-0}" == 1 ]]; then
    mkdir -p "$archive_dir/partial"
    printf 'root-created partial artifact\n' > "$archive_dir/partial/root-created.partial"
    exit 43
  fi
  exit 0
fi
echo "unexpected apt-get operation: $*" >&2
exit 91
EOF
chmod +x "$fake_bin/sudo"

PATH="$fake_bin:$PATH" \
APT_CALLS="$test_root/calls" \
APT_SOURCES="$test_root/sources" \
APT_INSTALL="$test_root/install" \
APT_WORK_DIR="$test_root/work-dir" \
UBUNTU_CODENAME=noble \
UBUNTU_ARCHIVE_KEYRING="$test_root/keyring.gpg" \
bash "$installer" clang mold

grep -F 'Dir::Etc::sourceparts=-' "$test_root/calls"
grep -F 'archive.ubuntu.com/ubuntu noble' "$test_root/sources"
grep -F 'security.ubuntu.com/ubuntu noble-security' "$test_root/sources"
if grep -Eqi 'chrome|google|third.party' "$test_root/sources"; then
  echo "third-party source leaked into isolated apt sources" >&2
  exit 1
fi
grep -F 'install -y clang mold' "$test_root/install"

if PATH="$fake_bin:$PATH" \
  APT_CALLS="$test_root/failing-calls" \
  APT_SOURCES="$test_root/failing-sources" \
  APT_INSTALL="$test_root/failing-install" \
  FAIL_UPDATE=1 \
  UBUNTU_CODENAME=noble \
  UBUNTU_ARCHIVE_KEYRING="$test_root/keyring.gpg" \
  bash "$installer" shellcheck; then
  echo "apt update failure was swallowed" >&2
  exit 1
fi
if [[ -e "$test_root/failing-install" ]]; then
  echo "apt install ran after update failure" >&2
  exit 1
fi

install_status=0
if PATH="$fake_bin:$PATH" \
  APT_CALLS="$test_root/install-failing-calls" \
  APT_SOURCES="$test_root/install-failing-sources" \
  APT_INSTALL="$test_root/install-failing-install" \
  APT_WORK_DIR="$test_root/install-failing-work-dir" \
  FAIL_INSTALL=1 \
  UBUNTU_CODENAME=noble \
  UBUNTU_ARCHIVE_KEYRING="$test_root/keyring.gpg" \
  bash "$installer" shellcheck; then
  echo "apt install failure was swallowed" >&2
  exit 1
else
  install_status=$?
fi
[[ "$install_status" == 43 ]]
install_failing_work_dir="$(<"$test_root/install-failing-work-dir")"
[[ ! -e "$install_failing_work_dir" ]]
grep -F 'rm -rf -- ' "$test_root/install-failing-calls" >/dev/null

ci_workflow="$repo_root/.github/workflows/ci.yml"
ploy_workflow="$repo_root/.github/workflows/ploy-ci.yml"
[[ "$(grep -cF 'install-ubuntu-packages.sh' "$ci_workflow")" == 3 ]]
[[ "$(grep -cF 'install-ubuntu-packages.sh' "$ploy_workflow")" == 6 ]]
if grep -nF 'sudo apt-get update' "$ci_workflow" "$ploy_workflow"; then
  echo "host workflow still uses the ambient apt source list" >&2
  exit 1
fi
[[ "$(grep -cE '^[[:space:]]+apt-get update$' "$ploy_workflow")" == 1 ]]
grep -F 'install-ubuntu-packages.sh" pkg-config libssl-dev libpq-dev clang lld' "$ploy_workflow" >/dev/null
grep -F 'install-ubuntu-packages.sh" pkg-config libssl-dev clang lld' "$ploy_workflow" >/dev/null
grep -F 'install-ubuntu-packages.sh" clang mold protobuf-compiler' "$ci_workflow" >/dev/null
grep -F 'install-ubuntu-packages.sh" shellcheck' "$ci_workflow" >/dev/null

echo "Ubuntu apt source isolation contract passed"
