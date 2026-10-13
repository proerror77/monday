#!/usr/bin/env bash
set -euo pipefail
script=$(cd "$(dirname "$0")" && pwd)/cargo-audit-summary.sh
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cat >"$work/findings.json" <<'JSON'
{"vulnerabilities":{"found":true,"count":2,"list":[
{"advisory":{"id":"RUSTSEC-2026-0338","cvss":null},"package":{"name":"quinn-proto","version":"0.11.16"}},
{"advisory":{"id":"RUSTSEC-2026-0340","cvss":"CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:N/A:H"},"package":{"name":"quinn-proto","version":"0.11.16"}}
]}}
JSON
bash "$script" "$work/findings.json" failure >"$work/report.txt"
grep -q 'result=failure, findings=2' "$work/report.txt"
grep -q 'RUSTSEC-2026-0338, RUSTSEC-2026-0340' "$work/report.txt"
grep -q 'quinn-proto 0.11.16' "$work/report.txt"
grep -q 'CVSS vectors available for 1/2' "$work/report.txt"
if grep -qE 'Critical=0|High=0' "$work/report.txt"; then exit 1; fi
printf '%s\n' '{"vulnerabilities":{"found":false,"count":0,"list":[]}}' >"$work/clean.json"
bash "$script" "$work/clean.json" success >"$work/clean.txt"
grep -q 'result=success, findings=0' "$work/clean.txt"
printf '%s\n' '{"vulnerabilities":{"found":false,"count":0,"list":[{}]}}' >"$work/inconsistent.json"
if bash "$script" "$work/inconsistent.json" success >/dev/null 2>&1; then exit 1; fi
printf '%s\n' '{}' >"$work/missing.json"
if bash "$script" "$work/missing.json" success >/dev/null 2>&1; then exit 1; fi
printf '%s\n' 'cargo audit summary contracts: ok'
