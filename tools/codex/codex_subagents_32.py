#!/usr/bin/env python3
# Set Codex subagent concurrency to 32 and optionally run a real concurrency test.
#
# Usage:
#   python codex_subagents_32.py
#   python codex_subagents_32.py --test

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile
import textwrap
from datetime import datetime
from pathlib import Path


SECTION_RE = re.compile(r"^\s*\[([^\]]+)\]\s*(?:#.*)?$")


def config_path() -> Path:
    codex_home = os.environ.get("CODEX_HOME")
    if codex_home:
        return Path(codex_home).expanduser() / "config.toml"
    return Path.home() / ".codex" / "config.toml"


def update_agents_section(text: str) -> str:
    lines = text.splitlines()
    section_start = None
    section_end = len(lines)

    for i, line in enumerate(lines):
        match = SECTION_RE.match(line)
        if match and match.group(1).strip() == "agents":
            section_start = i
            for j in range(i + 1, len(lines)):
                if SECTION_RE.match(lines[j]):
                    section_end = j
                    break
            break

    desired = {
        "enabled": "true",
        "max_concurrent_threads_per_session": "32",
    }

    if section_start is None:
        if lines and lines[-1].strip():
            lines.append("")
        lines.extend([
            "[agents]",
            "enabled = true",
            "max_concurrent_threads_per_session = 32",
        ])
        return "\n".join(lines) + "\n"

    body = lines[section_start + 1:section_end]
    seen = set()
    new_body = []

    for line in body:
        stripped = line.strip()

        if re.match(r"^max_threads\s*=", stripped):
            continue

        replaced = False
        for key, value in desired.items():
            if re.match(rf"^{re.escape(key)}\s*=", stripped):
                new_body.append(f"{key} = {value}")
                seen.add(key)
                replaced = True
                break

        if not replaced:
            new_body.append(line)

    for key, value in desired.items():
        if key not in seen:
            new_body.append(f"{key} = {value}")

    return "\n".join(
        lines[:section_start + 1] + new_body + lines[section_end:]
    ) + "\n"


def run_command(args: list[str], cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        args,
        cwd=str(cwd) if cwd else None,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=False,
    )


def configure() -> Path:
    path = config_path()
    path.parent.mkdir(parents=True, exist_ok=True)

    old_text = path.read_text(encoding="utf-8") if path.exists() else ""
    if path.exists():
        stamp = datetime.now().strftime("%Y%m%d-%H%M%S")
        backup = path.with_name(f"config.toml.backup-{stamp}")
        shutil.copy2(path, backup)
        print(f"Backup: {backup}")

    new_text = update_agents_section(old_text)
    path.write_text(new_text, encoding="utf-8")
    print(f"Updated: {path}")
    print("\nEffective global [agents] block:")

    in_agents = False
    for line in new_text.splitlines():
        match = SECTION_RE.match(line)
        if match:
            in_agents = match.group(1).strip() == "agents"
        if in_agents:
            print(line)

    return path


def validate_codex() -> None:
    codex = shutil.which("codex")
    if not codex:
        print(
            "\nCodex CLI was not found in PATH. The config file was updated, "
            "but CLI validation could not run. Fully restart Codex App."
        )
        return

    print(f"\nCodex executable: {codex}")
    version = run_command([codex, "--version"])
    print(version.stdout.strip())

    features = run_command([codex, "features", "list"])
    if features.returncode != 0:
        print("\nConfig validation failed:")
        print(features.stdout)
        raise SystemExit(features.returncode)

    print("Config load check: PASS")


def create_test_workspace(root: Path) -> None:
    worker = r"""#!/usr/bin/env python3
import os
import sys
import time
from pathlib import Path

worker_id = sys.argv[1]
log = Path("concurrency.log")
start_ns = time.time_ns()

with log.open("a", encoding="utf-8") as f:
    f.write(f"START {worker_id} {start_ns} {os.getpid()}\n")
    f.flush()

time.sleep(20)

end_ns = time.time_ns()
with log.open("a", encoding="utf-8") as f:
    f.write(f"END {worker_id} {end_ns} {os.getpid()}\n")
    f.flush()

print(f"worker {worker_id} complete")
"""

    analyzer = r"""#!/usr/bin/env python3
import sys
from pathlib import Path

path = Path(sys.argv[1] if len(sys.argv) > 1 else "concurrency.log")
if not path.exists():
    print("RESULT: no concurrency.log was created")
    raise SystemExit(2)

events = []
starts = set()
ends = set()

for raw in path.read_text(encoding="utf-8").splitlines():
    parts = raw.split()
    if len(parts) != 4:
        continue

    kind, worker_id, ts, pid = parts
    ts = int(ts)
    order = 0 if kind == "START" else 1
    events.append((ts, order, kind, worker_id))

    if kind == "START":
        starts.add(worker_id)
    elif kind == "END":
        ends.add(worker_id)

active = 0
peak = 0

for _, _, kind, _ in sorted(events):
    if kind == "START":
        active += 1
        peak = max(peak, active)
    else:
        active -= 1

print(f"RESULT: started={len(starts)} completed={len(ends)} peak_concurrent={peak}")

missing = sorted(starts - ends)
if missing:
    print("Missing END records:", ", ".join(missing))
"""

    (root / "worker.py").write_text(worker, encoding="utf-8")
    (root / "analyze.py").write_text(analyzer, encoding="utf-8")


def test_concurrency() -> None:
    codex = shutil.which("codex")
    if not codex:
        raise SystemExit("Cannot run the test because the codex CLI is not in PATH.")

    root = Path(tempfile.mkdtemp(prefix="codex-subagent-32-"))
    create_test_workspace(root)
    print(f"\nTest workspace: {root}")

    prompt = textwrap.dedent(
        """
        This is a strict subagent concurrency diagnostic.

        Spawn exactly 32 worker subagents, numbered 01 through 32.

        Requirements:
        - Issue all 32 spawn requests before waiting for any worker.
        - Each worker must execute exactly:
          python worker.py <its two-digit ID>
        - Do not run worker.py in the primary thread.
        - Do not reduce, merge, or optimize away workers.
        - After spawning, wait for every worker to finish.
        - If a spawn fails, preserve and report the exact error.
        - Finally execute:
          python analyze.py concurrency.log
        - Return the analyzer's RESULT line verbatim, plus any spawn-limit errors.
        """
    ).strip()

    output_file = root / "codex-output.txt"
    print("\nStarting 32-worker test. This consumes 32 subagent runs.")

    proc = subprocess.Popen(
        [
            codex,
            "exec",
            "--skip-git-repo-check",
            "-C",
            str(root),
            prompt,
        ],
        cwd=str(root),
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )

    assert proc.stdout is not None
    captured = []

    for line in proc.stdout:
        print(line, end="")
        captured.append(line)

    return_code = proc.wait()
    output_file.write_text("".join(captured), encoding="utf-8")

    print(f"\nCodex exit code: {return_code}")
    print(f"Raw output: {output_file}")

    analyzer = run_command([sys.executable, "analyze.py", "concurrency.log"], cwd=root)
    print(analyzer.stdout.strip())

    if analyzer.returncode == 0 and "peak_concurrent=32" in analyzer.stdout:
        print("TEST PASS: 32 workers overlapped concurrently.")
    else:
        print(
            "TEST NOT CONFIRMED: inspect codex-output.txt for AgentLimitReached, "
            "tool-call batching, permissions, or platform throttling."
        )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--test",
        action="store_true",
        help="Run a real 32-subagent concurrency test after configuration.",
    )
    args = parser.parse_args()

    configure()
    validate_codex()

    if args.test:
        test_concurrency()
    else:
        print(
            "\nConfiguration complete. Fully restart Codex App. "
            "Run this script again with --test to execute the 32-worker test."
        )


if __name__ == "__main__":
    main()
