# Controlled Attempt artifact identity

The host issuer serves an already admitted, leased PG Attempt. It never signs a
native grant, debits budget, issues an Agent capability or calls a cloud API.
`AttemptIdentityIssuer::issue_for_task` uses the controller's existing transaction.
The selected Task only locates the row. PG supplies tenant, TaskSpec, attempt,
fence, lease, deadline, native admission and stored public trust.

The issuer locks authority, task and original admission. It verifies the imported
native signature and trust identity. The current state must be launching or
running. The lease must remain active. Manual revocation closes issuance.
The expiry is the minimum of task deadline, all native effective times, native
expiry and 24 hours. It does not use the first lease expiry as token lifetime.
The gateway still checks current PG lease, fence and revocation on each write.

The configured gateway namespace must contain the derived artifact prefix.
The issuer uses OS entropy for a 32-byte random token. PG and the gateway
projection receive no raw token. The existing projection holds its SHA-256 and
exact AttemptWriter scope. Reader, Publisher and other Attempt entries remain
unchanged. All host projection writers share the same private sidecar lock.
Atomic replacement preserves the gateway's existing per-request hot reload.

A 0700 host state directory contains one durable journal per task/attempt/fence.
Late files use 0600. They are `artifact.token`, `native-admission.json` and optional
`tls.pem`. The native JSON is the actual verified imported signed projection.
TLS bytes come from the canonical, private host identity file. These files never
enter the static configuration hash. No signing or LLM key enters this journal.
The journal precedes capability publication and records hashes, never raw tokens.

`IssuedAttemptIdentity` has private fields and no Debug or Serialize implementation.
It exposes scope metadata and private late file bytes to the controlled launcher.
The launcher must independently read back its Secret UID and exact data before
mounting those bytes. A filename, source signature or local journal does not
prove a Kubernetes mount or scientific completion.

A retry validates the current PG scope and journal, then restores the same token.
A tightened deadline that no longer covers the stored identity fails closed.
The controller must reconcile the old resource and clean its owned identity.
It cannot overwrite an unknown Secret or reinterpret an older Attempt.
Explicit cleanup removes only the exact owned capability and verified files.
Foreign entries or changed ownership cause an error. Failed issuance removes its
owned local state when the projection can be safely read and reconciled.
A damaged projection remains closed and requires repair before recovery.

Tests use synthetic grants and a named disposable loopback PG database. They
verify current imported signatures, lease/fence/state, future native expiry,
namespace, same-transaction locking, recovery and owned cleanup. File fixtures
verify concurrency, permissions and rollback after a bounded projection fails.
These checks prove neither a live source ledger nor an actual Kubernetes Secret.

## Controlled Campaign launcher

The reconciler requires a configured host issuer for CEX Campaign tasks. The
static configuration must contain native trust matching the verified PG import.
The launcher creates one immutable late Secret under the deterministic Attempt
name. It reads back UID, scope and exact bytes before mounting the initialization
container. It checks lease and native deadline before provider mutations.

The task stores only public resource references. Raw credentials remain outside
PG. The recorded launch lease comes from the original Job environment, including
when creation returned an unknown outcome. Later heartbeats do not rewrite that
context. Recovery validates the original image, command, mounts and CPU resources.

After process-tree stop, cleanup verifies the owned Secret UID and data identity.
It deletes that Secret with a UID precondition and reads back absence. The issuer
then removes its own capability and private files. Historical grant expiry or
revocation cannot prevent mechanical cleanup. Cleanup does not restore authority.
Natural terminal Jobs and Pods remain available for independent source audit.

Loopback HTTP tests cover creation failure, recovery without a second Job, original
launch context, foreign UID rejection and exact Secret cleanup. These fixtures
create no cloud resources. Production deployment and audit-driven Job cleanup
remain separate requirements.
