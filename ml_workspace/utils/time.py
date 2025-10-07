"""
Time utilities for consistent timestamp handling across the project.

- ISO8601 parsing that tolerates trailing 'Z'
- UTC millisecond conversions
"""

from __future__ import annotations

from datetime import datetime, timezone
from typing import Optional


def to_ms(dt: datetime) -> int:
    """Convert a datetime to UTC epoch milliseconds.

    If `dt` lacks tzinfo, assume UTC.
    """
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    return int(dt.timestamp() * 1000)


def parse_iso8601(dt_str: str) -> datetime:
    """Parse ISO8601 string to a timezone-aware datetime.

    Accepts strings like "2025-09-03T00:00:00Z".
    """
    s = dt_str.strip()
    if s.endswith("Z"):
        s = s[:-1] + "+00:00"
    return datetime.fromisoformat(s)


def iso_to_ms(dt_str: str) -> int:
    """Parse ISO8601 string and return UTC epoch milliseconds."""
    return to_ms(parse_iso8601(dt_str))


def ms_to_iso(ms: int) -> str:
    """Convert UTC epoch milliseconds to ISO8601 string."""
    dt = datetime.fromtimestamp(ms / 1000, tz=timezone.utc)
    return dt.strftime('%Y-%m-%dT%H:%M:%SZ')

