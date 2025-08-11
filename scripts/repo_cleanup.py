#!/usr/bin/env python3
"""
Repository cleanup utility (dry-run by default).

Classifies directories/files into:
- remove_immediate: empty/orphan/artifacts safe to remove
- archive_then_remove: legacy/duplicate workspaces (tar.gz into backups/cleanup_TIMESTAMP)
- purge_artifacts: build caches and venvs in known subprojects

Usage:
  python scripts/repo_cleanup.py            # dry-run
  python scripts/repo_cleanup.py --apply    # perform actions

This tool only touches files within the repo root.
"""

from __future__ import annotations

import argparse
import shutil
import tarfile
from dataclasses import dataclass
from pathlib import Path
from time import strftime, gmtime


def find_repo_root(start: Path) -> Path:
    cur = start
    for _ in range(6):
        if (cur / ".git").exists():
            return cur
        if cur.parent == cur:
            break
        cur = cur.parent
    # fallback to cwd
    return Path.cwd().resolve()


REPO = find_repo_root(Path(__file__).resolve()).resolve()


@dataclass
class Action:
    kind: str  # remove | archive_remove | purge
    path: Path
    note: str = ""


def detect_actions() -> list[Action]:
    actions: list[Action] = []

    # Immediate removals (empty/orphan)
    empty_dirs = [
        REPO / "agno_compliance_fixes",
        REPO / "hft_ui_nextjs_new",
        REPO / "monitoring",
        REPO / "production_models",
        REPO / "shared",
    ]
    for d in empty_dirs:
        if d.exists():
            # Remove if empty or contains only empty subdirs
            if not any(d.rglob("*")):
                actions.append(Action("remove", d, "empty directory"))

    # Top-level artifacts
    for p in [REPO / "__pycache__", REPO / "dump.rdb", REPO / "node_modules"]:
        if p.exists():
            actions.append(Action("remove", p, "top-level artifact"))

    # Archive then remove (legacy/duplicates)
    for p in [REPO / "ops_workspace", REPO / "hft-master-workspace", REPO / "hft_demo"]:
        if p.exists():
            actions.append(Action("archive_remove", p, "legacy/duplicate workspace"))

    # Purge artifacts within subprojects
    # Rust
    for p in [REPO / "rust_hft/target", REPO / "rust_hft/.venv", REPO / "rust_hft/logs"]:
        if p.exists():
            actions.append(Action("purge", p, "rust artifacts/venv/logs"))
    # Python venv caches
    for p in [REPO / "ml_workspace/.venv", REPO / "ops_workspace/.venv"]:
        if p.exists():
            actions.append(Action("purge", p, "python venv"))
    # UI build caches
    for p in [REPO / "hft_ui_nextjs/.next", REPO / "hft_ui_nextjs/node_modules"]:
        if p.exists():
            actions.append(Action("purge", p, "ui build cache"))

    return actions


def archive_then_remove(path: Path, backup_dir: Path) -> None:
    backup_dir.mkdir(parents=True, exist_ok=True)
    tar_path = backup_dir / f"{path.name}.tgz"
    with tarfile.open(tar_path, "w:gz") as tar:
        tar.add(path, arcname=path.name)
    shutil.rmtree(path, ignore_errors=True)


def main() -> int:
    parser = argparse.ArgumentParser(description="Repo cleanup utility")
    parser.add_argument("--apply", action="store_true", help="apply changes")
    args = parser.parse_args()

    actions = detect_actions()
    if not actions:
        print("No actions detected.")
        return 0

    print("Planned actions (dry-run)" if not args.apply else "Applying actions")
    for a in actions:
        print(f"- [{a.kind}] {a.path.relative_to(REPO)} :: {a.note}")

    if not args.apply:
        print("\nRun with --apply to execute. A backup will be created for archived items.")
        return 0

    backup_dir = REPO / "backups" / f"cleanup_{strftime('%Y%m%d_%H%M%S', gmtime())}"
    for a in actions:
        try:
            if a.kind == "remove":
                if a.path.is_dir():
                    shutil.rmtree(a.path, ignore_errors=True)
                elif a.path.exists():
                    a.path.unlink(missing_ok=True)
            elif a.kind == "archive_remove":
                archive_then_remove(a.path, backup_dir)
            elif a.kind == "purge":
                shutil.rmtree(a.path, ignore_errors=True)
        except Exception as e:
            print(f"! Failed {a.kind} {a.path}: {e}")

    print(f"Done. Backup saved under: {backup_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
