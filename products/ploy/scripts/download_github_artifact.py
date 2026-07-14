#!/usr/bin/env python3
"""Download and extract a GitHub Actions artifact by name.

This intentionally uses the GitHub REST API instead of actions/download-artifact
so downstream jobs can consume artifacts from previous workflow runs without
depending on the action's Node download path on self-hosted runners.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import sys
import tempfile
import time
import urllib.error
import urllib.request
import zipfile
from pathlib import Path
from typing import Any


def request_json(url: str, token: str) -> dict[str, Any]:
    req = urllib.request.Request(
        url,
        headers={
            "Accept": "application/vnd.github+json",
            "Authorization": f"Bearer {token}",
            "X-GitHub-Api-Version": "2022-11-28",
        },
    )
    with urllib.request.urlopen(req, timeout=60) as resp:
        return json.loads(resp.read().decode("utf-8"))


class NoRedirectHandler(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):  # type: ignore[no-untyped-def]
        return None


def download_file(url: str, token: str, destination: Path) -> None:
    req = urllib.request.Request(
        url,
        headers={
            "Accept": "application/vnd.github+json",
            "Authorization": f"Bearer {token}",
            "X-GitHub-Api-Version": "2022-11-28",
        },
    )
    opener = urllib.request.build_opener(NoRedirectHandler)
    try:
        resp = opener.open(req, timeout=60)
    except urllib.error.HTTPError as err:
        if err.code not in {301, 302, 303, 307, 308} or "Location" not in err.headers:
            raise
        blob_url = err.headers["Location"]
        resp = urllib.request.urlopen(blob_url, timeout=300)

    with resp:
        with destination.open("wb") as handle:
            shutil.copyfileobj(resp, handle)


def safe_extract(zip_path: Path, output_dir: Path) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    output_root = output_dir.resolve()
    with zipfile.ZipFile(zip_path) as archive:
        for member in archive.infolist():
            target = (output_dir / member.filename).resolve()
            if target != output_root and output_root not in target.parents:
                raise RuntimeError(f"refusing to extract path outside output dir: {member.filename}")
        archive.extractall(output_dir)


def copy_tree_contents(source_dir: Path, output_dir: Path) -> None:
    if not source_dir.is_dir():
        raise RuntimeError(f"strip prefix is not a directory in artifact: {source_dir}")

    output_dir.mkdir(parents=True, exist_ok=True)
    for item in source_dir.iterdir():
        target = output_dir / item.name
        if item.is_dir():
            if target.exists():
                shutil.rmtree(target)
            shutil.copytree(item, target)
        else:
            shutil.copy2(item, target)


def required_missing(output_dir: Path, required_paths: list[str]) -> list[str]:
    return [path for path in required_paths if not (output_dir / path).exists()]


def safe_component(value: str) -> str:
    cleaned = re.sub(r"[^A-Za-z0-9._-]+", "-", value).strip("-")
    return cleaned or "value"


def cache_payload_dir(cache_root: Path, repo: str, run_id: str, artifact: dict[str, Any], strip_prefix: str) -> Path:
    strip_hash = hashlib.sha256(strip_prefix.encode("utf-8")).hexdigest()[:12]
    key = "--".join(
        [
            safe_component(repo.replace("/", "__")),
            f"run-{safe_component(run_id)}",
            f"artifact-{artifact['id']}",
            safe_component(str(artifact["name"])),
            f"strip-{strip_hash}",
        ]
    )
    return cache_root / key / "payload"


def prune_cache(cache_root: Path, ttl_days: int) -> None:
    if ttl_days <= 0 or not cache_root.is_dir():
        return
    cutoff = time.time() - ttl_days * 24 * 60 * 60
    for child in cache_root.iterdir():
        try:
            if child.is_dir() and child.stat().st_mtime < cutoff:
                shutil.rmtree(child)
        except OSError:
            continue


def copy_output_to_cache(output_dir: Path, payload_dir: Path, metadata: dict[str, Any]) -> None:
    cache_entry = payload_dir.parent
    tmp_entry = cache_entry.with_name(f"{cache_entry.name}.tmp-{os.getpid()}")
    shutil.rmtree(tmp_entry, ignore_errors=True)
    tmp_payload = tmp_entry / "payload"
    tmp_payload.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(output_dir, tmp_payload)
    (tmp_entry / "metadata.json").write_text(json.dumps(metadata, indent=2, sort_keys=True), encoding="utf-8")
    shutil.rmtree(cache_entry, ignore_errors=True)
    tmp_entry.rename(cache_entry)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--run-id", required=True, help="Workflow run id that owns the artifact")
    parser.add_argument("--name", required=True, help="Artifact name to download")
    parser.add_argument("--output-dir", required=True, help="Directory to extract the artifact into")
    parser.add_argument(
        "--strip-prefix",
        default="",
        help="Optional artifact subdirectory whose contents should become the output root",
    )
    parser.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY"))
    parser.add_argument("--api-url", default=os.environ.get("GITHUB_API_URL", "https://api.github.com"))
    parser.add_argument("--token", default=os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN"))
    parser.add_argument(
        "--cache-dir",
        default="",
        help="Optional directory for runner-local extracted artifact cache",
    )
    parser.add_argument(
        "--cache-ttl-days",
        type=int,
        default=14,
        help="Delete cache entries older than this many days; set 0 to disable cleanup",
    )
    parser.add_argument(
        "--require",
        action="append",
        default=[],
        help="Relative path that must exist after extraction; may be repeated",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if not args.repo:
        print("GITHUB_REPOSITORY is required or pass --repo", file=sys.stderr)
        return 2
    if not args.token:
        print("GITHUB_TOKEN or GH_TOKEN is required", file=sys.stderr)
        return 2

    artifacts_url = (
        f"{args.api_url}/repos/{args.repo}/actions/runs/{args.run_id}/artifacts"
        f"?name={args.name}&per_page=100"
    )
    try:
        payload = request_json(artifacts_url, args.token)
    except urllib.error.HTTPError as err:
        print(f"failed to list artifacts: HTTP {err.code}: {err.read().decode('utf-8')}", file=sys.stderr)
        return 1

    artifacts = [
        item
        for item in payload.get("artifacts", [])
        if item.get("name") == args.name and not item.get("expired", False)
    ]
    if not artifacts:
        found = ", ".join(item.get("name", "<unknown>") for item in payload.get("artifacts", []))
        print(f"artifact not found: {args.name}; found: {found or '<none>'}", file=sys.stderr)
        return 1

    artifact = sorted(artifacts, key=lambda item: item.get("created_at", ""))[-1]
    output_dir = Path(args.output_dir)
    cache_dir = Path(args.cache_dir).expanduser() if args.cache_dir else None
    payload_dir: Path | None = None
    if cache_dir is not None:
        prune_cache(cache_dir, args.cache_ttl_days)
        payload_dir = cache_payload_dir(cache_dir, args.repo, args.run_id, artifact, args.strip_prefix)
        if payload_dir.is_dir() and not required_missing(payload_dir, args.require):
            shutil.rmtree(output_dir, ignore_errors=True)
            copy_tree_contents(payload_dir, output_dir)
            print(
                "reused cached artifact "
                f"name={artifact['name']} id={artifact['id']} size={artifact.get('size_in_bytes', 0)} "
                f"from={payload_dir} to={output_dir}"
            )
            return 0

    with tempfile.TemporaryDirectory(prefix="ploy-artifact-") as tmp:
        zip_path = Path(tmp) / "artifact.zip"
        extract_dir = Path(tmp) / "extract"
        try:
            download_file(artifact["archive_download_url"], args.token, zip_path)
        except urllib.error.HTTPError as err:
            print(f"failed to download artifact: HTTP {err.code}: {err.read().decode('utf-8')}", file=sys.stderr)
            return 1
        if args.strip_prefix:
            safe_extract(zip_path, extract_dir)
            shutil.rmtree(output_dir, ignore_errors=True)
            copy_tree_contents(extract_dir / args.strip_prefix, output_dir)
        else:
            safe_extract(zip_path, output_dir)

    missing = required_missing(output_dir, args.require)
    if missing:
        print(f"artifact extracted but required paths are missing: {', '.join(missing)}", file=sys.stderr)
        return 1

    if payload_dir is not None:
        metadata = {
            "repo": args.repo,
            "run_id": args.run_id,
            "artifact_id": artifact["id"],
            "artifact_name": artifact["name"],
            "artifact_size": artifact.get("size_in_bytes", 0),
            "strip_prefix": args.strip_prefix,
            "created_at": artifact.get("created_at"),
            "cached_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        }
        copy_output_to_cache(output_dir, payload_dir, metadata)

    print(
        "downloaded artifact "
        f"name={artifact['name']} id={artifact['id']} size={artifact.get('size_in_bytes', 0)} "
        f"to={output_dir}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
