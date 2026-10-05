# Native Campaign platform producer

This code starts with the canonical finalized Campaign submission and its rendered Job.
It does not accept a caller's budget reservation, Root grant, approval or receipt hash.

`mission dispatch prepare-platform` reuses the existing admission inspection.
It verifies the original signature, current approval, family and Study scope.
It durably reserves the inspected trials, Job seconds and LLM tokens.
It publishes native receipts and reads their exact bytes back independently.
It then returns the current source budget inspection.
This command does not export a signed platform admission or launch a Job.

The source store exposes opaque `VerifiedCampaignPlatformBudget` and `VerifiedCampaignPlatformExport` values.
They have no deserializer or public constructor.
The export callback reacquires the native approval, family and Study guards.
It requires the exact `PlatformTransferred` record and its published receipt.
It preserves the original deadline and full charge after unknown export or PG import outcomes.
The operation hash is `canonical_json_hash` of the original structured operation ID.
It is not a newly named retry.

A complete signing path also requires the independently verified Build release and executable bytes.
It must bind the exact Run, Task, source, image, resources, configuration and verified development collection.
The CEX app inspector supplies the opaque finalized-request/data-equivalence object.
Caller-selected expected hashes and a whole dataset relabeled as Train are rejected.
The initial canonical Campaign gets one platform attempt.
Another attempt requires a new native reservation under the same cumulative budget and original deadline.

Only the host-owned native witness key may sign the resulting exact admission.
That key stays outside DB, JSON, logs and Agent access.
The prebuilt CLI does not run Cargo while holding signing credentials.
Data or body-equivalence failure must stop export before signing.

The source store also derives `VerifiedCampaignPlatformRevocation` from actual authenticated Root or Study receipts.
Every item targets one existing platform transfer and retains the source effective time.
Its reason identity hashes the exact authenticated publication object, not caller metadata.
Unpublished transfer or revocation receipts block projection.
Multiple Root and Study reasons remain separate; the earliest effective constraint must govern execution.
The downstream signed revocation receiver and effective-time guards are separate integration work.
This interface does not issue or revoke arbitrary row-wide approvals.

Platform terminal settlement still requires independent task, Job, Pod, receipt and stopped-process evidence.
A worker hash or claimed success cannot refund or settle the native reservation.
The legacy dispatch and settlement paths remain excluded after platform transfer.
That complete terminal bridge and signed data-bound export are not claimed by the preparation interface.

Tests use synthetic native grants, published receipt peers and local source ledgers.
They verify debit retention, ownership, expiration, revocation and callback ordering.
They do not prove cloud execution, real native budget issuance or scientific completion.
