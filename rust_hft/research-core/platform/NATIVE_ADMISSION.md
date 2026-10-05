# Native admission receiver

The operator imports a signed native reservation projection using
`researchctl register-native-admission SIGNED_NATIVE_RESERVATION` with
`MONDAY_RESEARCH_NATIVE_ADMISSION_TRUST_FILE`. Native reservation public-key trust
is distinct from software release trust. Signing keys never belong to PG or an
Agent. The generic research API cannot issue a grant or import a projection.

The projection binds tenant, immutable Run, exact TaskSpec, verified Build release,
original configuration, native operation, grant/approval/reservation/transfer
receipts, trial charge, total reserved Job seconds and expiry. PG requires the
registered tenant-owned Run and verified Build to match. One source operation
cannot authorize another request. Imports are immutable and exact retransmission
is idempotent. Plausible receipt-hash rows without this signed import remain audit
history and cannot launch work. The receiver checks signature, exact identities,
expiry and native revocation before launch and terminal publication. The upload
permit also checks tenant and expiry under its original authority/task/admission
locks, lease/fence/deadline and cancellation guards.

Install additive migrations in this order: `postgres.sql`,
`verified_build_release.sql`, `session_deliveries.sql`, `artifact_gateway.sql`,
`native_admission.sql`. The last migration replaces the original permit's exact
signature and preserves its safeguards. No migration or CLI import activates PG
or a backend. The native importer needs SELECT on registered Runs/Builds/releases
and SELECT/INSERT on admissions and native_admission_imports; it cannot be the
Agent or worker role. Prepare workers receive only the native import request,
tenant and expiry columns. Keep installation and production cutover separate.

Native Campaign transfer retains the source ledger's existing reservation and
full charge. The native approval/family/study and published receipt guards order
transfer against claim and revocation. It prevents the old dispatcher and legacy
settlement path from owning that operation. Unknown PG import or execution does
not mean zero consumption or release budget. Only independent platform terminal
and stop evidence may settle it through a separately implemented bridge.

The actual producer must validate the original signed native grant and current
approval, debit cumulative budgets in the existing source ledger, bind the exact
finalized Campaign request and typed prepared-input roles, durably transfer its
execution ownership, and independently read back the source receipts before
signing. This receiver and transfer primitive do not implement that complete
producer or prove data equivalence. The Campaign shared-input collection and
canonical worker adapter remain required: search workers must not receive
selection or holdout bytes, and a whole native dataset must not be relabeled as a
Train view. Source metadata and signatures are not scientific terminal evidence.

Tests use synthetic signatures and a named disposable loopback PG database. They
prove rejection, persistence and ownership semantics, never real budget issuance,
cloud execution, data preparation or scientific completion.
