#!/usr/bin/env python3
"""Run the Tango market-data gap audit through Aliyun Cloud Assistant."""

from __future__ import annotations

import base64
import os
import shlex
import subprocess
import sys
import time
from pathlib import Path
from typing import Any


def require_env(name: str) -> str:
    value = os.environ.get(name)
    if not value:
        raise SystemExit(f"{name} is required")
    return value


def run(cmd: list[str], *, capture: bool = False) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        check=True,
        text=True,
        capture_output=capture,
    )


def run_json(cmd: list[str]) -> dict[str, Any]:
    import json

    result = run(cmd, capture=True)
    return json.loads(result.stdout)


def shell_quote(value: str) -> str:
    return shlex.quote(value)


def remote_script() -> str:
    deploy_root = require_env("DEPLOY_ROOT")
    run_full = require_env("RUN_FULL")
    full_lookback_hours = require_env("FULL_LOOKBACK_HOURS")
    bucket_minutes = require_env("BUCKET_MINUTES")
    symbols = require_env("SYMBOLS")
    required_sources = require_env("REQUIRED_SOURCES")
    gate_mode = require_env("GATE_MODE")
    github_run_id = require_env("GITHUB_RUN_ID")
    github_run_attempt = require_env("GITHUB_RUN_ATTEMPT")
    bucket = require_env("ALIYUN_OSS_BUCKET")
    region = require_env("DEPLOY_OSS_REGION")
    object_prefix = f"reports/market-data-gap-audit/{github_run_id}/{github_run_attempt}"

    return f"""#!/usr/bin/env bash
set -euo pipefail

export OSS_ACCESS_KEY_ID={shell_quote(require_env("ALIYUN_OSS_ACCESS_KEY_ID"))}
export OSS_ACCESS_KEY_SECRET={shell_quote(require_env("ALIYUN_OSS_ACCESS_KEY_SECRET"))}
export OSS_ENDPOINT={shell_quote(f"https://oss-{region}-internal.aliyuncs.com")}

ensure_ossutil() {{
  export PATH="/usr/local/bin:/usr/bin:/bin:${{PATH}}"
  if command -v ossutil >/dev/null 2>&1; then
    return 0
  fi
  curl -fsSL https://gosspublic.alicdn.com/ossutil/install.sh | bash
  export PATH="/usr/local/bin:/usr/bin:/bin:${{PATH}}"
  command -v ossutil >/dev/null 2>&1
}}

run_audit() {{
  local audit_kind="$1"
  local lookback_hours="$2"
  local remote_file="${{workdir}}/${{audit_kind}}.json"
  mkdir -p "$(dirname "${{remote_file}}")" "{deploy_root}/reports/market-data-gap-audit"
  test -x "{deploy_root}/scripts/audit_market_data_gaps.py"
  "{deploy_root}/scripts/audit_market_data_gaps.py" \\
    --lookback-hours "${{lookback_hours}}" \\
    --bucket-minutes {shell_quote(bucket_minutes)} \\
    --symbols {shell_quote(symbols)} \\
    --required-sources {shell_quote(required_sources)} \\
    --gate-mode {shell_quote(gate_mode)} \\
    --statement-timeout-seconds 20 \\
    --psql-timeout-seconds 30 \\
    --format json \\
    --fail-on never > "${{remote_file}}"
  cp "${{remote_file}}" "{deploy_root}/reports/market-data-gap-audit/${{audit_kind}}-latest.json"
  ossutil cp "${{remote_file}}" "oss://{bucket}/{object_prefix}/${{audit_kind}}.json" \\
    -i "${{OSS_ACCESS_KEY_ID}}" -k "${{OSS_ACCESS_KEY_SECRET}}" -e "${{OSS_ENDPOINT}}"
}}

workdir="{deploy_root}/tmp/market-data-gap-audit-{github_run_id}-{github_run_attempt}"
rm -rf "${{workdir}}"
mkdir -p "${{workdir}}"
ensure_ossutil
run_audit quick 1
if [ {shell_quote(run_full)} = "true" ]; then
  run_audit full {shell_quote(full_lookback_hours)}
fi
"""


def download_artifacts() -> None:
    region = require_env("DEPLOY_OSS_REGION")
    bucket = require_env("ALIYUN_OSS_BUCKET")
    key_id = require_env("ALIYUN_OSS_ACCESS_KEY_ID")
    key_secret = require_env("ALIYUN_OSS_ACCESS_KEY_SECRET")
    run_id = require_env("GITHUB_RUN_ID")
    attempt = require_env("GITHUB_RUN_ATTEMPT")
    run_full = require_env("RUN_FULL")
    object_prefix = f"reports/market-data-gap-audit/{run_id}/{attempt}"
    out_dir = Path("artifacts/market-data-gap-audit")
    out_dir.mkdir(parents=True, exist_ok=True)
    endpoint = f"https://oss-{region}.aliyuncs.com"

    names = ["quick"]
    if run_full == "true":
        names.append("full")
    for name in names:
        run(
            [
                "ossutil",
                "cp",
                f"oss://{bucket}/{object_prefix}/{name}.json",
                str(out_dir / f"{name}.json"),
                "-i",
                key_id,
                "-k",
                key_secret,
                "-e",
                endpoint,
            ]
        )


def main() -> int:
    region = require_env("DEPLOY_OSS_REGION")
    instance_id = require_env("TANGO_1_1_INSTANCE_ID")
    command_name = f"ploy-market-data-audit-{require_env('GITHUB_RUN_ID')}"
    command_content = remote_script()

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
            "600",
            "--CommandContent",
            command_content,
            "--InstanceId.1",
            instance_id,
        ]
    )
    invoke_id = result["InvokeId"]
    print(f"Cloud Assistant audit invoke_id={invoke_id}")

    for _ in range(120):
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
        exit_code = row.get("ExitCode")
        print(f"Cloud Assistant audit status={status} exit={exit_code}")
        if status in {"Pending", "Running"}:
            time.sleep(5)
            continue

        output = base64.b64decode(row.get("Output") or b"").decode("utf-8", "replace")
        if output:
            print(output)
        if status == "Success" and exit_code == 0:
            download_artifacts()
            return 0
        return 1

    print("Cloud Assistant audit did not finish before polling timeout", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
