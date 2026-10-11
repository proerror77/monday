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

## Provider mutations still hold locks

`launch`, `stop` and late-identity cleanup retain task/admission serialization and
the shared authority lock. This orders a provider mutation against cancellation
and revocation. Removing those locks alone could commit cancellation before a
late POST creates the worker. An unresolved launch initially retains Launching.
The existing stop path can accept an absent resource and empty Pod list without
durable evidence that a timed-out POST has finished. That leaves a late-create
race; this readback change does not establish provider-operation quiescence.

The existing provider client has a 15-second timeout per HTTP request. This is
not a 15-second limit for the entire mutation. A Campaign launch can make up to
seven requests: static configuration, late identity and Job creation/readback.
Stop plus late-identity cleanup can make up to five. Local identity and PG work
adds to those request bounds. Pause and new claims can still wait for these
mutation sections. This change does not claim that every I/O path is lock-free.

Further separation requires a durable operation claim before releasing locks.
That claim must bind the original task, attempt, fence, provider name and known
UID. Cancellation records stop intent while retaining the allocation. A timeout
must reconcile that same operation before retrying a POST or accepting absence.
An unknown request cannot mint a replacement resource or refund its allocation.
Such a change needs its own launch/cancel/stop interleaving tests.

## Validation

The focused unit test checks heartbeat renewal during real artifact reads and
dropping pending I/O after losing its revision. The existing PostgreSQL contract
uses an isolated `monday_foundation_test` database and a delayed loopback GET.
It checks unrelated claims/cancel/pause, exact-revision conflicts, expiry,
replacement fences, actual connection termination and concurrent renewal.
The GET fixture and PG checks prove lock and state behavior; they do not prove
production OSS connectivity or a real Kubernetes mutation.

The implementation reuses PR #1352's short-claim and heartbeat work, then extends
it to Build downloads and provider observation with exact revision checks.
