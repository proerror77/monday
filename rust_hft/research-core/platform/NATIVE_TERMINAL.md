# Native terminal readback

`researchctl terminal-snapshot TENANT REQUEST` reads one terminal request from PG.
It uses one read-only, repeatable-read transaction. It returns the fixed Run,
signed admission, stored public trust, terminal revision, preceding execution
event and optional result. It checks their request, tenant, Attempt and receipt
bindings. Historical expiry and revocation remain readable.

The command does not issue authority or settle a budget. An independently governed
source reader must authenticate the command transport and configured public trust.
It must read the original provider resources and scientific outputs. Deserializing
this snapshot does not create verified terminal evidence.

The reconciler retains naturally completed or failed Jobs and their original Pods.
It acknowledges stop only when one original owner Pod has terminated every
declared container, including configuration initialization. It rejects incomplete
lists, missing statuses, restarted containers, active processes and identity drift.
The source reader applies its own stricter execution and scientific checks.

Cancellation and timeout still use UID-bound foreground deletion. Missing provider
resources do not prove a scientific result to the source reader. Unknown outcomes
retain their budget charge. Queued requests without a reconciled execution event
do not enter this readback contract.

Retained resources require task-owned cleanup after independent audit. This change
does not implement that cleanup, configure a deployment, or run scientific work.
Local tests use synthetic provider documents and a disposable PG database.
