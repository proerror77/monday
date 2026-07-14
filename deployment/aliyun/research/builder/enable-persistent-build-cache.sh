#!/usr/bin/env bash
set -euo pipefail

cache_root=/build-cache
config_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

mountpoint -q "$cache_root" || {
  echo "$cache_root must be a mounted persistent disk" >&2
  exit 1
}
if [[ -e /etc/docker/daemon.json ]] && ! cmp -s "$config_dir/docker-daemon.json" /etc/docker/daemon.json; then
  echo "/etc/docker/daemon.json already has unrelated settings; merge data-root manually" >&2
  exit 1
fi

sudo install -d -m 0755 "$cache_root/docker"
sudo install -D -m 0644 "$config_dir/docker-daemon.json" /etc/docker/daemon.json
sudo systemctl restart docker
test "$(docker info --format '{{.DockerRootDir}}')" = "$cache_root/docker"
