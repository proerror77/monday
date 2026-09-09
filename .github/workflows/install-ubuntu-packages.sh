#!/usr/bin/env bash
set -euo pipefail

if (($# == 0)); then
  echo "usage: $0 PACKAGE..." >&2
  exit 2
fi

release="${UBUNTU_CODENAME:-}"
if [[ -z "$release" && -r /etc/os-release ]]; then
  # shellcheck disable=SC1091
  . /etc/os-release
  release="${VERSION_CODENAME:-}"
fi
if [[ ! "$release" =~ ^[a-z0-9][a-z0-9.-]*$ ]]; then
  echo "unable to determine a safe Ubuntu release codename" >&2
  exit 1
fi

keyring="${UBUNTU_ARCHIVE_KEYRING:-/usr/share/keyrings/ubuntu-archive-keyring.gpg}"
if [[ ! -r "$keyring" ]]; then
  echo "Ubuntu archive keyring is missing: $keyring" >&2
  exit 1
fi

work_dir="$(mktemp -d)"

cleanup() {
  local status=$?
  local cleanup_status=0

  # apt may leave _apt-owned partial files behind, so clean this task-owned
  # hierarchy through the same scoped privilege boundary used for apt-get.
  if [[ -e "$work_dir" ]]; then
    sudo rm -rf -- "$work_dir" || cleanup_status=$?
  fi
  if ((status == 0 && cleanup_status != 0)); then
    status=$cleanup_status
  fi
  exit "$status"
}
trap cleanup EXIT

source_list="$work_dir/sources.list"
state_dir="$work_dir/state"
mkdir -p "$state_dir/lists/partial" "$state_dir/archives/partial"
chmod 0755 \
  "$work_dir" \
  "$state_dir" \
  "$state_dir/lists" \
  "$state_dir/lists/partial" \
  "$state_dir/archives" \
  "$state_dir/archives/partial"

cat > "$source_list" <<EOF
deb [signed-by=$keyring] http://archive.ubuntu.com/ubuntu $release main restricted universe multiverse
deb [signed-by=$keyring] http://archive.ubuntu.com/ubuntu ${release}-updates main restricted universe multiverse
deb [signed-by=$keyring] http://security.ubuntu.com/ubuntu ${release}-security main restricted universe multiverse
EOF

apt_options=(
  -o "Dir::Etc::sourcelist=$source_list"
  -o 'Dir::Etc::sourceparts=-'
  -o "Dir::State::lists=$state_dir/lists"
  -o "Dir::Cache::archives=$state_dir/archives"
)

sudo apt-get "${apt_options[@]}" update
sudo apt-get "${apt_options[@]}" install -y "$@"
