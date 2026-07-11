#!/usr/bin/env bash
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

failures=0

report() {
  printf '%s\n' "$1" >&2
  failures=1
}

forbidden_paths=(
  "config/.env.shared"
  "config/.env.unified"
  "rust_hft/config/secrets.yaml"
  "rust_hft/clickhouse_credentials.txt"
  "rust_hft/deployment/k8s/secrets.yaml"
  "rust_hft/hft-admin-ssh-20250926144355.pem"
  "rust_hft/hft-collector-key-new.pem"
  "rust_hft/hft-collector-key.pem"
  "rust_hft/k8s/bitget/clickhouse-secret.yaml"
)

for path in "${forbidden_paths[@]}"; do
  if git ls-files --error-unmatch "$path" >/dev/null 2>&1; then
    report "tracked secret file still present: $path"
  fi
done

key_hits="$(git ls-files | rg '(^|/)[^/]+\.(pem|key|p12|pfx|crt)$' || true)"
if [[ -n "$key_hits" ]]; then
  report "tracked key material detected:"
  printf '%s\n' "$key_hits" >&2
fi

python3 - <<'PY' || failures=1
import pathlib
import re
import subprocess
import sys

root = pathlib.Path.cwd()
tracked = subprocess.run(
    ["git", "ls-files"],
    check=True,
    capture_output=True,
    text=True,
).stdout.splitlines()

skip_prefixes = ("docs/", "rust_hft/docs/", "rust_hft/tests/", "rust_hft/specs/")
skip_suffixes = (".example", ".sample", ".md", ".rs")
patterns = [
    re.compile(
        r"^\s*(?:export\s+)?(?:Environment=)?[\"']?"
        r"([A-Za-z0-9_]*(?:SECRET|PASSWORD|PASSWD|PASSPHRASE|PRIVATE_KEY|API_KEY|ACCESS_KEY|SIGNING_KEY|PAGERDUTY_KEY|WEBHOOK(?:_URL)?|TOKEN|CREDENTIAL)[A-Za-z0-9_]*)"
        r"\s*=\s*(.+?)[\"']?\s*$",
    ),
    re.compile(r"^\s*(api_secret|passphrase|secret_key|private_key|password)\s*:\s*(.+?)\s*$"),
    re.compile(r"^\s*([A-Za-z0-9_-]*(?:api-key|api-secret|passphrase|password|private-key|webhook-url|pagerduty-key))\s*:\s*(.+?)\s*$", re.IGNORECASE),
    re.compile(r"^\s*Password:\s*(.+?)\s*$"),
]
allowed_prefixes = ("${", "$", "<", "CHANGE_ME", "YOUR_", "your_", "example_", "EXAMPLE_", "REPLACE_", "replace_")
allowed_exact = {"", "\"\"", "''", "null", "None"}
private_key_header = re.compile(r"^-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----$")

for sample in (
    "Environment=CLICKHOUSE_PASSWORD=CHANGE_ME_LITERAL",
    'Environment="PAGERDUTY_KEY=CHANGE_ME_LITERAL"',
    "ALERT_WEBHOOK_URL=CHANGE_ME_LITERAL",
    "BITGET_SECRET_KEY=CHANGE_ME_LITERAL",
):
    if not patterns[0].match(sample):
        print(f"secret scanner self-check failed for {sample.split('=', 1)[0]}", file=sys.stderr)
        sys.exit(1)

violations = []
for rel in tracked:
    if rel.startswith(skip_prefixes) or rel.endswith(skip_suffixes):
        continue
    if not rel.endswith((".sh", ".txt", ".yaml", ".yml", ".env", ".shared", ".unified")):
        continue
    path = root / rel
    if not path.is_file():
        continue
    text = path.read_text(errors="ignore").splitlines()
    for lineno, line in enumerate(text, 1):
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if private_key_header.match(stripped):
            violations.append((rel, lineno, "private key marker"))
            continue
        for pattern in patterns:
            match = pattern.match(line)
            if not match:
                continue
            value = match.group(2 if pattern.groups >= 2 else 1).strip().strip('"').strip("'")
            if value in allowed_exact or value.startswith(allowed_prefixes):
                break
            violations.append((rel, lineno, match.group(1)))
            break

if violations:
    for rel, lineno, label in violations:
        print(f"suspicious tracked secret in {rel}:{lineno} ({label})", file=sys.stderr)
    sys.exit(1)
PY

if [[ "$failures" -ne 0 ]]; then
  exit 1
fi

echo "tracked secret check passed"
