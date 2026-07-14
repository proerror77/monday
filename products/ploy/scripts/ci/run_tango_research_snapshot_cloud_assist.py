#!/usr/bin/env python3
"""Run research-snapshot compilation through Aliyun Cloud Assistant.

This is the fallback path for times when GitHub Actions can reach Aliyun APIs
and OSS but cannot complete an SSH banner exchange with tango-1-1.
"""

from __future__ import annotations

import json
import base64
import os
import shlex
import subprocess
import sys
import tarfile
import time
from pathlib import Path
from typing import Any


def require_env(name: str) -> str:
    value = os.environ.get(name)
    if not value:
        raise SystemExit(f"{name} is required")
    return value


def env(name: str, default: str = "") -> str:
    return os.environ.get(name, default)


def run(cmd: list[str], *, capture: bool = False) -> subprocess.CompletedProcess[str]:
    return subprocess.run(cmd, check=True, text=True, capture_output=capture)


def run_json(cmd: list[str]) -> dict[str, Any]:
    return json.loads(run(cmd, capture=True).stdout)


def q(value: str) -> str:
    return shlex.quote(value)


def remote_script() -> str:
    deploy_root = require_env("DEPLOY_ROOT")
    run_id = require_env("GITHUB_RUN_ID")
    attempt = require_env("GITHUB_RUN_ATTEMPT")
    bucket = require_env("ALIYUN_OSS_BUCKET")
    region = require_env("DEPLOY_OSS_REGION")
    object_prefix = f"research-snapshot/{run_id}/{attempt}"
    audit_script_b64 = base64.b64encode(
        Path("scripts/audit_market_data_gaps.py").read_bytes()
    ).decode("ascii")

    return f"""#!/usr/bin/env bash
set -euo pipefail

export OSS_ACCESS_KEY_ID={q(require_env("ALIYUN_OSS_ACCESS_KEY_ID"))}
export OSS_ACCESS_KEY_SECRET={q(require_env("ALIYUN_OSS_ACCESS_KEY_SECRET"))}
export OSS_ENDPOINT={q(f"https://oss-{region}-internal.aliyuncs.com")}

ensure_ossutil() {{
  export PATH="/usr/local/bin:/usr/bin:/bin:${{PATH}}"
  if command -v ossutil >/dev/null 2>&1; then
    return 0
  fi
  curl -fsSL https://gosspublic.alicdn.com/ossutil/install.sh | bash
  export PATH="/usr/local/bin:/usr/bin:/bin:${{PATH}}"
  command -v ossutil >/dev/null 2>&1
}}

write_audit_md() {{
  python3 - <<'PY'
import json
from pathlib import Path

root = Path("research-snapshot")
payload = json.loads((root / "data-gap-audit.json").read_text(encoding="utf-8"))
status = payload.get("overall_status", "unknown")
lines = [
    "# Scoped Data Audit",
    "",
    f"- Gate: `{q(env("SNAPSHOT_DATA_GATE", "never"))}`",
    f"- Gate mode: `{{payload.get('gate_mode', 'unknown')}}`",
    f"- Overall: `{{status}}`",
    f"- Required sources: `{{','.join(payload.get('required_sources') or [])}}`",
    f"- Lookback: `{{payload.get('lookback_hours')}}h`",
    f"- Audit window: `{{payload.get('audit_window_start_ts') or '<lookback-start>'}} -> {{payload.get('audit_window_end_ts') or '<lookback-end>'}}`",
    "",
    "| Source | Status | Reasons |",
    "| --- | --- | --- |",
]
for item in payload.get("gap_audits", []) + payload.get("window_audits", []):
    lines.append(
        f"| `{{item.get('source_id')}}` | `{{item.get('status', 'unknown')}}` | "
        f"{{'; '.join(item.get('reasons') or [])}} |"
    )
(root / "data-gap-audit.md").write_text("\\n".join(lines) + "\\n", encoding="utf-8")
PY
}}

workdir={q(f"{deploy_root}/tmp/research-snapshot-cloud-assist-{run_id}-{attempt}")}
rm -rf "${{workdir}}"
mkdir -p "${{workdir}}/research-snapshot" "${{workdir}}/workflow-scripts"
cd "${{workdir}}"
ensure_ossutil

python3 - <<'PY'
import base64
from pathlib import Path

payload = {audit_script_b64!r}
path = Path("workflow-scripts/audit_market_data_gaps.py")
path.write_bytes(base64.b64decode(payload.encode("ascii")))
path.chmod(0o755)
PY

set -a
. {q(deploy_root)}/.env
set +a
: "${{PLOY_DATABASE__URL:?PLOY_DATABASE__URL missing in {deploy_root}/.env}}"

test -x workflow-scripts/audit_market_data_gaps.py
PLOY_DATABASE__URL="${{PLOY_DATABASE__URL}}" \\
  ./workflow-scripts/audit_market_data_gaps.py \\
    --lookback-hours {q(env("SNAPSHOT_AUDIT_LOOKBACK_HOURS", "168"))} \\
    --start-ts {q(require_env("SNAPSHOT_START_TS"))} \\
    --end-ts {q(require_env("SNAPSHOT_END_TS"))} \\
    --bucket-minutes 5 \\
    --symbols {q(require_env("SNAPSHOT_SYMBOLS"))} \\
    --required-sources {q(require_env("REQUIRED_SOURCES"))} \\
    --orderbook-archive-root {q(env("SNAPSHOT_ORDERBOOK_ARCHIVE_ROOT", "/opt/ploy/data/lake"))} \\
    --statement-timeout-seconds 20 \\
    --psql-timeout-seconds 30 \\
    --format json \\
    --fail-on never > research-snapshot/data-gap-audit.json
write_audit_md

audit_status="$(python3 - <<'PY'
import json
print(json.load(open("research-snapshot/data-gap-audit.json", encoding="utf-8")).get("overall_status", "unknown"))
PY
)"
gate={q(env("SNAPSHOT_DATA_GATE", "never"))}
if [ "${{gate}}" = "critical" ] && {{ [ "${{audit_status}}" = "critical" ] || [ "${{audit_status}}" = "unknown" ]; }}; then
  printf '2\\n' > research-snapshot/compile-status.txt
  tar -cf research-snapshot.tar research-snapshot
  ossutil cp research-snapshot.tar "oss://{bucket}/{object_prefix}/research-snapshot.tar" \\
    -i "${{OSS_ACCESS_KEY_ID}}" -k "${{OSS_ACCESS_KEY_SECRET}}" -e "${{OSS_ENDPOINT}}"
  exit 2
fi
if [ "${{gate}}" = "warn" ] && [ "${{audit_status}}" != "ok" ]; then
  printf '2\\n' > research-snapshot/compile-status.txt
  tar -cf research-snapshot.tar research-snapshot
  ossutil cp research-snapshot.tar "oss://{bucket}/{object_prefix}/research-snapshot.tar" \\
    -i "${{OSS_ACCESS_KEY_ID}}" -k "${{OSS_ACCESS_KEY_SECRET}}" -e "${{OSS_ENDPOINT}}"
  exit 2
fi

compiler={q(deploy_root)}/bin/research-snapshot-compile
test -x "${{compiler}}"
deribit_args=()
if [ {q(require_env("INCLUDE_DERIBIT"))} != "true" ]; then
  deribit_args+=(--skip-deribit)
fi
set +e
timeout 3600 "${{compiler}}" \\
  --db-url "${{PLOY_DATABASE__URL}}" \\
  --symbols {q(require_env("SNAPSHOT_SYMBOLS"))} \\
  --start-ts {q(require_env("SNAPSHOT_START_TS"))} \\
  --end-ts {q(require_env("SNAPSHOT_END_TS"))} \\
  --stake-usd {q(require_env("SNAPSHOT_STAKE_USD"))} \\
  --lob-sample-secs {q(require_env("SNAPSHOT_LOB_SAMPLE_SECS"))} \\
  --pm-book-sample-secs {q(require_env("SNAPSHOT_PM_BOOK_SAMPLE_SECS"))} \\
  --pm-book-archive-dir {q(env("SNAPSHOT_ORDERBOOK_ARCHIVE_ROOT", "/opt/ploy/data/lake") + "/orderbook_snapshots")} \\
  --observation-sample-secs {q(require_env("SNAPSHOT_OBSERVATION_SAMPLE_SECS"))} \\
  --max-quote-age-secs {q(require_env("SNAPSHOT_MAX_QUOTE_AGE_SECS"))} \\
  --output-dir research-snapshot \\
  --optimizer-data-dir {q(require_env("SNAPSHOT_OPTIMIZER_DATA_DIR"))} \\
  --data-requirements {q(require_env("REQUIRED_SOURCES"))} \\
  --data-audit-status "${{audit_status}}" \\
  --data-audit-report data-gap-audit.json \\
  "${{deribit_args[@]}}" \\
  2>&1 | tee research-snapshot/compile.log
compile_status="${{PIPESTATUS[0]}}"
set -e
printf '%s\\n' "${{compile_status}}" > research-snapshot/compile-status.txt
tar -cf research-snapshot.tar research-snapshot
ossutil cp research-snapshot.tar "oss://{bucket}/{object_prefix}/research-snapshot.tar" \\
  -i "${{OSS_ACCESS_KEY_ID}}" -k "${{OSS_ACCESS_KEY_SECRET}}" -e "${{OSS_ENDPOINT}}"
exit "${{compile_status}}"
"""


def download_snapshot_tar() -> None:
    region = require_env("DEPLOY_OSS_REGION")
    bucket = require_env("ALIYUN_OSS_BUCKET")
    key_id = require_env("ALIYUN_OSS_ACCESS_KEY_ID")
    key_secret = require_env("ALIYUN_OSS_ACCESS_KEY_SECRET")
    run_id = require_env("GITHUB_RUN_ID")
    attempt = require_env("GITHUB_RUN_ATTEMPT")
    object_path = f"research-snapshot/{run_id}/{attempt}/research-snapshot.tar"
    out = Path("artifacts/research-snapshot.tar")
    out.parent.mkdir(parents=True, exist_ok=True)
    run(
        [
            "ossutil",
            "cp",
            f"oss://{bucket}/{object_path}",
            str(out),
            "-i",
            key_id,
            "-k",
            key_secret,
            "-e",
            f"https://oss-{region}.aliyuncs.com",
        ]
    )
    root = Path("artifacts/research-snapshot")
    if root.exists():
        import shutil

        shutil.rmtree(root)
    root.parent.mkdir(parents=True, exist_ok=True)
    with tarfile.open(out) as archive:
        archive.extractall(root.parent)


def main() -> int:
    region = require_env("DEPLOY_OSS_REGION")
    instance_id = require_env("TANGO_1_1_INSTANCE_ID")
    command_name = f"ploy-research-snapshot-{require_env('GITHUB_RUN_ID')}"
    result = run_json(
        [
            "aliyun",
            "ecs",
            "RunCommand",
            "--RegionId",
            region,
            "--Type",
            "RunShellScript",
            "--Name",
            command_name,
            "--Timeout",
            "4200",
            "--CommandContent",
            remote_script(),
            "--InstanceId.1",
            instance_id,
        ]
    )
    invoke_id = result["InvokeId"]
    print(f"Cloud Assistant research snapshot invoke_id={invoke_id}")

    for _ in range(150):
        status_result = run_json(
            [
                "aliyun",
                "ecs",
                "DescribeInvocationResults",
                "--RegionId",
                region,
                "--InvokeId",
                invoke_id,
            ]
        )
        rows = status_result["Invocation"]["InvocationResults"]["InvocationResult"]
        if not rows:
            time.sleep(5)
            continue
        row = rows[0]
        status = row.get("InvocationStatus")
        exit_code = int(row.get("ExitCode", -1))
        print(f"Cloud Assistant research snapshot status={status} exit={exit_code}")
        if status in {"Pending", "Running"}:
            time.sleep(5)
            continue

        output = base64.b64decode((row.get("Output") or "").encode()).decode(
            "utf-8",
            "replace",
        )
        if output:
            print(output)
        if status != "Success":
            return 1
        break
    else:
        print(
            "Cloud Assistant research snapshot did not finish before polling timeout",
            file=sys.stderr,
        )
        return 1

    try:
        download_snapshot_tar()
    except Exception as exc:  # noqa: BLE001 - fallback evidence is best effort.
        print(f"failed to download research snapshot tar from OSS: {exc}", file=sys.stderr)
        return 1

    status_path = Path("artifacts/research-snapshot/compile-status.txt")
    manifest_path = Path("artifacts/research-snapshot/manifest.json")
    if exit_code != 0:
        return exit_code if exit_code > 0 else 1
    if not status_path.exists() or status_path.read_text(encoding="utf-8").strip() != "0":
        return 1
    if not manifest_path.exists():
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
