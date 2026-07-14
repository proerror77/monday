"""Build libpq environment variables from an explicit PostgreSQL URL.

Keeping credentials out of the ``psql`` argument vector avoids exposing them in
process listings and gives subprocess callers a fixed executable/argument shape.
"""

from __future__ import annotations

import os
from collections.abc import Mapping
from urllib.parse import parse_qs, unquote, urlsplit


_PG_KEYS = {
    "PGAPPNAME",
    "PGCONNECT_TIMEOUT",
    "PGDATABASE",
    "PGHOST",
    "PGPASSWORD",
    "PGPORT",
    "PGSSLMODE",
    "PGUSER",
}
_QUERY_KEYS = {
    "application_name": "PGAPPNAME",
    "connect_timeout": "PGCONNECT_TIMEOUT",
    "sslmode": "PGSSLMODE",
}


def psql_environment(
    database_url: str,
    base_environment: Mapping[str, str] | None = None,
) -> dict[str, str]:
    """Return a subprocess environment for one PostgreSQL connection URL."""

    parsed = urlsplit(database_url)
    if parsed.scheme not in {"postgres", "postgresql"}:
        raise ValueError("database URL must use postgres:// or postgresql://")
    if parsed.fragment:
        raise ValueError("database URL fragments are not supported")

    try:
        port = parsed.port
    except ValueError as exc:
        raise ValueError("database URL has an invalid port") from exc

    database = unquote(parsed.path.lstrip("/"))
    if not database:
        raise ValueError("database URL must include a database name")

    query = parse_qs(parsed.query, keep_blank_values=True, strict_parsing=True)
    unsupported = sorted(set(query) - set(_QUERY_KEYS))
    if unsupported:
        raise ValueError(f"unsupported database URL options: {', '.join(unsupported)}")
    if any(len(values) != 1 or not values[0] for values in query.values()):
        raise ValueError("database URL options must have one non-empty value")

    environment = dict(os.environ if base_environment is None else base_environment)
    for key in _PG_KEYS:
        environment.pop(key, None)

    environment["PGDATABASE"] = database
    if parsed.hostname:
        environment["PGHOST"] = parsed.hostname
    if port is not None:
        environment["PGPORT"] = str(port)
    if parsed.username is not None:
        environment["PGUSER"] = unquote(parsed.username)
    if parsed.password is not None:
        environment["PGPASSWORD"] = unquote(parsed.password)
    for option, values in query.items():
        environment[_QUERY_KEYS[option]] = values[0]
    return environment
