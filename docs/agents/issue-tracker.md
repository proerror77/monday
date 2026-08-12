# Issue tracker

Monday uses GitHub Issues for work that must survive a session, coordinate
multiple owners, or record production authority. Local investigation and small
changes do not need an issue.

## One outcome, one issue

- Keep one issue for one behavior or runtime outcome.
- Put attempts, failures, cleanup receipts, and final evidence on that issue.
- Open another issue only when the behavior, target, authority, or independently
  reviewable change differs. An owner handoff stays on the same issue.
- Use GitHub's native parent and blocked-by relationships when they help current
  coordination; do not duplicate them as mandatory body sections.

## Pull requests

Use one visible relationship in the PR body:

- `Closes #N` when merging to `main` completes the code contract.
- `Refs #N` for partial, tracking, runtime, or stacked work.
- `None` when no issue is needed.

Runtime and tracking issues close only from their own terminal evidence, never
from a code merge.

## Runtime outcomes

Before a live mutation, record the exact target, one controller, candidate and
rollback identities, stop rules, and success/readback criteria. The controller
must clean up on every exit path. A failed attempt may run again under the same
issue only after its cause or relevant input changed and the new hypothesis is
stated.

Use `gh issue view <number> --comments` for the current contract and history.
Use `--body-file` when publishing multiline Markdown.
