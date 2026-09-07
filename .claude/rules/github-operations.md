# GitHub Operations

Use the root [delivery authority](../../AGENTS.md#delivery-authority) and
[issue lifecycle](../../docs/agents/issue-tracker.md). Use structured `--json`
readback and `--body-file` for multiline publication. Execute the requested
operation directly and handle its actual result.

Classify failures before retrying: authentication/permission errors require the
missing access; rate limits and transient service/network failures allow bounded
backoff; conflicts require fresh identity/state readback. After an uncertain
write, look for the created object before repeating it. Reuse stable identity to
avoid duplicate issues, comments, or PRs. Stop repeated unchanged failures under
the task's wait policy; report the real error, not a generic login instruction.
