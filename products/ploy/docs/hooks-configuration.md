# Hook configuration inside Monday

The standalone PLOY agent/session hook configuration was intentionally excluded from
the migration. PLOY does not ship an active project-local Claude hook configuration
inside Monday.

Use the Monday root instructions and the developer tooling configured by the current
workspace. Do not recreate former session hooks, notification scripts, or agent state
without a separate repository-level review.

Current product validation is enforced by `.github/workflows/ploy-ci.yml`, focused
local tests, actionlint, dependency audit, and tracked-secret scanning.
