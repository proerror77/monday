# Research readback transactions

Task claims hold the authority row exclusively only while checking quota and
committing the original attempt, fence and deadline. A second claimant rechecks
capacity after acquiring that lock. Launching, running and stopping tasks all
consume quota until process termination is verified.

Build object downloads, provider observation, receipts and checkpoint reads use
a committed `ReadbackSnapshot`. The snapshot contains the complete Task and its
revision. Creating it releases authority, task and admission row locks before
external I/O begins. A readback is evidence, not provider mutation authority.

Heartbeat transactions accept only that exact revision and Task. Each successful
pulse returns a replacement snapshot with its own new revision. Reacquisition
checks the same identity, current PG clock, lease, original absolute deadline,
authority witnesses and native/manual revocation. It can narrow a deadline but
cannot reset it. Cancellation, another writer or an expired fence discards the
readback; losing a heartbeat drops its in-flight I/O future.

## Provider creation and cancellation

Ordinary reconciler provider GET, POST and DELETE calls run outside PG row
locks. `ProviderMutation` reacquires the current Task for short journal changes;
it never writes a pre-I/O snapshot over a later cancellation or revision.

A claim atomically inserts one NotStarted execution operation, plus a second
identity operation for CEX Campaigns. The key binds task, Attempt, fence,
cluster, namespace and original resource name. Immediately before its first
POST, the controller persists Pending and the exact request SHA. Job/Sandbox
records retain the original non-secret body, including launch lease, deadline,
checkpoint and Secret UID. Secret records retain metadata and data SHA only.
No credential body enters PG.

After that commit, the same controller reacquires authority, Task and admission,
checks current lease/deadline/revocations and verifies the committed revision.
Only that successful NotStarted-to-Pending caller can receive one in-memory
FirstSend permit. It releases the transaction before HTTP. Cancellation before
permit issuance records NotSent only when that original caller knows it has
not invoked HTTP. A restarted or competing controller cannot infer NotSent.
A crash after Pending commits but before send is therefore conservatively
unknown; it has no automatic absence-based recovery.

FirstSend issuance is the admission ordering point. Cancellation or revocation
can commit while that already-authorized request is in flight, including the
small interval before send. It moves the Task toward Stopping and retains its
allocation. It does not authorize another POST. HTTP timeouts, 408/409/429 and
all 5xx remain Pending. Only explicit API rejection statuses resolve Rejected.
The HTTP client disables automatic retries.

Recovery only GETs the original resource. It compares all authored execution
fields against the stored first request, tolerates only established API defaults
and quantity normalization, and fixes the observed UID. Heartbeat expiry changes
cannot regenerate an original request hash. A matching UID is committed before
DELETE, whose UID precondition prevents deleting a replacement resource.
An absent Pending Job, Sandbox or Secret remains unresolved. Terminal/requeue
updates have a SQL guard requiring every expected create to be resolved; even
a caller bypassing the Rust stop path cannot refund an unknown create.
Known-never-started, original-caller NotSent, definite rejection or observed UID
may pass the create barrier, but resource/Pod termination and identity cleanup
must still pass their own readbacks. This does not add a new retry policy.

The additive migration and minimal reconciler-only grants are offline assets.
Install them paused after all old writers are quiescent. Existing active attempts
with no operation record remain unknown: there is no fabricated never-sent
backfill, credential publication, or production migration in this change.
Independent operator evidence is required to resolve historical/crash ambiguity.

The provider client retains a 15-second timeout per HTTP request, not a global
15-second transaction. The change removes those network waits from ordinary
reconciler transactions. Kubernetes `activeDeadlineSeconds` is relative to
provider creation, so GET/POST or server delays can move its wall-clock end.
Preserving the original request does not prove an exact absolute process cutoff.
PG lease/deadline and native credential checks govern their admission/publication
boundaries; actual process termination still requires independent provider/Pod
readback. This change does not claim that a late worker cannot begin computing.
Operator-only terminal-retirement and native Session startup have separate
serialization contracts and are not changed here.

## Validation

The focused unit test checks heartbeat renewal during real artifact reads and
dropping pending I/O after losing its revision. The existing PostgreSQL contract
uses an isolated `monday_foundation_test` database and a delayed loopback GET.
It checks unrelated claims/cancel/pause, exact-revision conflicts, expiry,
replacement fences, actual connection termination and concurrent renewal.
Real disposable PG tests also cover a slow POST with concurrent cancellation,
crash before send, the intent/permit cancellation interval, competing controllers,
definite rejection, unknown 5xx, expiry, immutable request identity and settlement
rejection. Loopback provider fixtures cover late Job/Sandbox/Secret appearance,
API defaulting, body/UID substitution and one POST per operation. Independent PG
role tests reject non-reconciler writes. These tests prove local protocol and
transaction behavior, not production OSS or Kubernetes acceptance.

The implementation reuses PR #1352's short-claim and heartbeat work, then extends
it to Build downloads and provider observation with exact revision checks.
