#!/usr/bin/env python3
from __future__ import annotations

import json
import logging
import os
from typing import Optional


class JsonFormatter(logging.Formatter):
    def format(self, record: logging.LogRecord) -> str:
        payload = {
            "level": record.levelname,
            "name": record.name,
            "message": record.getMessage(),
        }
        if record.exc_info:
            payload["exc_info"] = self.formatException(record.exc_info)
        if record.stack_info:
            payload["stack_info"] = self.formatStack(record.stack_info)
        return json.dumps(payload, ensure_ascii=False)


def configure_logging(level: Optional[str] = None, json_output: Optional[bool] = None) -> None:
    """Configure root logger for the workspace.

    Env overrides:
    - LOG_LEVEL (e.g., DEBUG, INFO)
    - LOG_JSON (true/false)
    """
    lvl = (level or os.getenv("LOG_LEVEL") or "INFO").upper()
    use_json = json_output if json_output is not None else (str(os.getenv("LOG_JSON", "false")).lower() in {"1", "true", "yes", "y", "on"})

    logger = logging.getLogger()
    logger.setLevel(lvl)

    # Clear existing handlers (idempotent re-configure)
    for h in list(logger.handlers):
        logger.removeHandler(h)

    handler = logging.StreamHandler()
    if use_json:
        handler.setFormatter(JsonFormatter())
    else:
        fmt = "[%(levelname)s] %(name)s: %(message)s"
        handler.setFormatter(logging.Formatter(fmt))
    logger.addHandler(handler)

