#!/usr/bin/env bash
set -euo pipefail

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

assert_file_contains() {
  local file="$1"
  local needle="$2"
  if [[ ! -f "$file" ]]; then
    fail "expected file '$file' to exist"
  fi
  if ! grep -Fq "$needle" "$file"; then
    fail "expected '$file' to contain '$needle'"
  fi
}

assert_file_not_contains() {
  local file="$1"
  local needle="$2"
  if [[ ! -f "$file" ]]; then
    return 0
  fi
  if grep -Fq "$needle" "$file"; then
    fail "expected '$file' not to contain '$needle'"
  fi
}
