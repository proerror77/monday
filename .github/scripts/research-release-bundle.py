#!/usr/bin/env python3
"""CI-only transport: keep executable modes, reject arbitrary archive paths."""
import os
from pathlib import Path
import sys
import tarfile

BINARIES = (
    "hft-backtest", "alpha-harness", "lob-pit-materializer",
    "binance-market-tape-slicer", "binance-replay-parquet-materializer",
    "research-orchestrator", "researchctl", "research-prepare",
    "clickhouse-analytics-materializer", "monday-prediction-research",
    "monday-prediction-evaluator", "monday-prediction-snapshot",
)
FILES = {"research-image-release.json": (0o644, 1024 * 1024)}
FILES.update({"research-bin/" + name: (0o755, 512 * 1024 * 1024) for name in BINARIES})
MAX_BYTES = 1024 * 1024 * 1024


def main():
    mode, archive_name, directory_name = sys.argv[1:]
    archive, directory = Path(archive_name), Path(directory_name)
    if mode == "pack":
        if archive.exists():
            raise ValueError("bundle output already exists")
        actual = {str(p.relative_to(directory)) for p in directory.rglob("*") if not p.is_dir()}
        if actual != set(FILES):
            raise ValueError("release file set differs from fixed executable manifest")
        total = 0
        with tarfile.open(archive, "x", format=tarfile.USTAR_FORMAT) as bundle:
            for name, (mode_bits, maximum) in FILES.items():
                source = directory / name
                info = source.lstat()
                if not source.is_file() or source.is_symlink() or not 0 < info.st_size <= maximum:
                    raise ValueError("invalid release file: " + name)
                if name.startswith("research-bin/") and info.st_mode & 0o777 != mode_bits:
                    raise ValueError("release executable mode is not 0755: " + name)
                total += info.st_size
                if total > MAX_BYTES:
                    raise ValueError("release bundle exceeds byte budget")
                header = tarfile.TarInfo(name)
                header.size, header.mode = info.st_size, mode_bits
                with source.open("rb") as stream:
                    bundle.addfile(header, stream)
    elif mode == "unpack":
        if directory.exists() or archive.is_symlink() or not 0 < archive.stat().st_size <= MAX_BYTES + 32768:
            raise ValueError("bundle/output boundary invalid")
        with tarfile.open(archive, "r:") as bundle:
            members = bundle.getmembers()
            if len(members) != len(FILES) or {m.name for m in members} != set(FILES):
                raise ValueError("bundle file set is not the exact release manifest")
            total = 0
            for member in members:
                mode_bits, maximum = FILES[member.name]
                if not member.isreg() or member.linkname or member.mode != mode_bits or not 0 < member.size <= maximum:
                    raise ValueError("unsafe bundle member: " + member.name)
                total += member.size
            if total > MAX_BYTES:
                raise ValueError("release bundle exceeds byte budget")
            directory.mkdir(mode=0o700, parents=True)
            (directory / "research-bin").mkdir(mode=0o700)
            for member in members:
                # Never extract archive paths or metadata; write only fixed regular files.
                destination = directory / member.name
                flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW
                with os.fdopen(os.open(destination, flags, 0o600), "wb") as output:
                    source = bundle.extractfile(member)
                    remaining = member.size
                    while remaining:
                        data = source.read(min(remaining, 1024 * 1024))
                        if not data:
                            raise ValueError("truncated release member: " + member.name)
                        output.write(data)
                        remaining -= len(data)
                destination.chmod(member.mode)
    else:
        raise ValueError("expected pack or unpack")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, tarfile.TarError) as error:
        sys.exit("research release bundle rejected: " + str(error))
