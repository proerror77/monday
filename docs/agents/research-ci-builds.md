# Research CI planning and compilation

Prediction CI selects checks from the current source change first. Those checks
can start while `research-image-scope` recovers pending image delivery. Binary
production and smoke wait for that recovery. The final CI gate requires both
planning stages and every selected check to succeed.

Image recovery always reads authenticated product publication history. A later
documentation change must not discard earlier unpublished software. Only a valid
offline archive allowance causes a second history read for signed Builds.
Without that allowance, `research_archive_pending_product=not-evaluated` and
`research_archive_base_sha=null`. These values mean unknown, not published.
An approved archive history failure still blocks research delivery and the gate.

The history reader retains product-specific baselines, original run attempts and
partial image successes. An image does not prove a signed Build. A successful
no-op publisher does not advance either baseline.

Use `test-main-research-scope.sh`, `test-ci-monitor-scope.sh` and
`test-acr-publish-workflow.sh` for these contracts. Test doubles prove planning
and rejection behavior; they do not measure hosted CI latency.
