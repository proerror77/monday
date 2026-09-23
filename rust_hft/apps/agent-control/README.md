# Engineering agent task contract

`hft-agent-control` is slice 1a of Monday's AX migration: a small, non-default
workspace library for engineering-task transition and evidence validation. It
has no executable, provider SDK, network, process execution, database, or
dependency on research, governance or trading crates. It does not take over the
shell helper, AX, or the ACK Campaign controller.

The model records one immutable task specification, an original absolute
deadline, capability references, resource units, invocation identities and
cumulative model/CPU accounting. Each new invocation reserves its declared
allowance before returning `StartDecision::Submit`. A repeated start returns
`Reconcile`; an unknown submission cannot become a fresh invocation. Successful
submission is not evidence of task completion.

An authenticated `NotSubmittedReceiptV1` can terminate an invocation that was
rejected before execution or authoritatively confirmed never submitted. It binds
the complete original invocation and observation time. Only this confirmed unused
reservation is released; the operation identity, attempted-invocation count and
failure receipt remain. The task cannot return to Pending or automatically retry.
A missing response, failed lookup or timeout is still unresolved. Any subsequent
task requires new admission and authority; this receipt does not grant it.

Pause requires a matching execution's quiescence observation and committed file
checkpoint. The model checks identity, lineage, time ordering and the supplied
artifact bytes against SHA-256. Recovery starts a new process from that exact
file checkpoint. There is no promise of process-memory or model-session restore.
Consumption, reservations and external operation identities stay in the control
record, outside checkpoint contents. Unknown usage or external effects prevent
new spending. A matching usage receipt records authenticated consumption even
while external effects remain unresolved. In that case the result is
`AwaitingExternalReconciliation` and the original reservation stays held. It is
released only after both usage and every external resolution are known; neither
arrival order can discard usage or refund an unknown effect.
Authenticated usage above a reservation or global cap is retained and fully
charged, with a sticky `overrun` failure that blocks further spend and verified
success. Replaying the same receipt is idempotent; it cannot charge twice or be
replaced with a lower figure. Single-invocation usage is `u64`, cumulative usage
is `u128`, and at most `u32` invocations can be admitted, so cumulative accounting
remains exact even beyond `u64::MAX`. Terminal observations remain readable after
an overrun.

A terminal receipt records the command's actual exit status and output/log
digests. Exit zero leaves the outcome unverified. A separate readback must match
the complete terminal record and observed output digest before completion.
`VerifiedOutcome::Negative` preserves a valid negative finding; it cannot turn
a nonzero process exit into successful completion. This is an engineering-agent
contract, not a research result or a substitute Campaign accounting ledger.

## Trust and integration limits

- Receipt values are **trusted observations supplied by the adapter**. This
  library checks their consistency; it does not establish their authenticity,
  prove that a process stopped, read a remote object, or make a checkpoint durable.
  The caller must independently authenticate the provider, usage, artifact and
  verifier evidence. Hashing supplied bytes alone does not prove cloud readback.
- `revision` rejects stale transitions within one authoritative model instance.
  It is not a lock or durable compare-and-swap. The future adapter must atomically
  compare and persist the state before issuing effects. Losing or restoring this
  state from a workspace snapshot invalidates its guarantees. The control type
  is not `Clone`; constructing two instances is not safe multi-writer admission.
- `OwnershipClaimsV1` models the shared conflict rule for task and legacy-lease
  claims. Task bindings (and consequently all invocation/receipt identities)
  include repository, filesystem namespace, canonical worktree path, branch, PR
  and write scope. Within one repository, same writer, branch, PR or overlapping
  write scope conflicts. The same filesystem namespace and worktree path also
  conflict even with different workspace IDs. Distinct actors may both use
  `/workspace` when their filesystem namespaces and other scopes are disjoint.
  The caller must authenticate the repository and filesystem identities and
  canonicalize worktree/write paths, including symlinks, mounts and filesystem
  case aliases. This model performs only lexical checks, not filesystem lookup.
  Write scopes explicitly support repository-wide access, literal files and
  subtrees using normalized ASCII paths. Globs, traversal and other unsupported
  forms fail closed. Translation of existing shell globs belongs to slice 1b.
  This model is not connected to the existing ownership store; slice 1b must
  integrate one atomic admission path that both entrypoints obey. There is no
  claim release, automatic retry or takeover in this slice.
- The caller must refresh current authority and call `revoke` before further
  effects when authority is revoked. A stored boolean is not a live revocation
  service. Observation, cleanup and historical readback remain possible.
- Capability names and immutable packet digests are references, not secrets.
  There is no command or secret payload field, and no claim that types can detect
  credentials inserted into arbitrary strings. Secret containment and enforced
  resource/egress limits belong to later platform acceptance.
- There is no wire/storage schema or serializer yet. The `V1` names identify the
  initial Rust contract; checkpoint/terminal descriptors must not be mistaken
  for already authenticated or durably published receipt formats.

## Verification

From `rust_hft`, run `cargo test --locked -p hft-agent-control` and
`cargo clippy --locked -p hft-agent-control --all-targets --no-deps -- -D warnings`.
The deterministic fake provider counts intended submissions after a lost response.
Other counterexamples cover stale observers, shared ownership conflicts, unknown
external effects, invalid pause/checkpoint evidence, accounting preservation,
identity tampering, expiry, revocation and independent completion readback.
Review regressions also cover cross-name scope conflicts, separate actor
filesystems, unsupported globs, exact non-submission evidence and overrun
accounting across the `u64` boundary.
These tests prove model behavior, not real concurrent exclusion, isolation,
storage durability, AX feasibility or a completed research run.
