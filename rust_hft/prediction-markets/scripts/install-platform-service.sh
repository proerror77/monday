#!/usr/bin/env bash
set -euo pipefail

printf '%s\n' \
  "The standalone PLOY platform installer is retired and disabled in Monday." \
  "Monday owns deployment and execution authority; use a separately reviewed Monday deployment change." >&2
exit 78
