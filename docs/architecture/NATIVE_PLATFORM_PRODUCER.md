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

## Functional ownership and completion review

This review separates source authority, executable publication and actual scientific consumption.
The source anchor for the preparation interfaces is `267f5c02e`.
CEX's `d36e05286` checkpoint provides the verified-data inspector; its complete worker wiring is pending.
Root owns the new CexCampaign/PG input-kind contract and independent revocation receiver.

| Subfunction | Caller and authority owner | Actual consumer | Verified boundary | Remaining gap |
| --- | --- | --- | --- | --- |
| Publication | Native software issuer and release verifier; software keys and registry policy own release authority | Verified Build registration; reconciler executable readback; released worker executable | Separate source/build/image/executable identity and signed release checks | Actual publication/registry readback must be recorded per immutable release; it proves no budget or scientific result |
| Admission | Controlled native importer; original native Root, approval, family and Study remain scientific authority | PG submit, launch, preparation permit, upload permit and terminal commit | Signature/tenant/request/operation/expiry bindings; unsigned and reused-operation rejection | Complete source producer and data-bound import are still being connected |
| Native budget | Canonical finalized dispatch inspection; native source ledger owns cumulative debit and ownership | prepare-platform; guarded opaque budget/export callbacks | Published authentic receipts, full charge retention, original deadlines, exact transfer and revocation ordering | Exact signed Run/Task/data export and controlled PG import remain pending |
| Revocation | Native Root/Study revocation receipts; host witness projects only an existing transfer | Source export guards; future effective-time PG launch/prepare/upload/terminal guards | Published reason objects and scheduled times; separate Root/Study causes preserve earlier constraints | Domain-separated receiver and source-to-PG sync need integration; request revocation is not arbitrary approval issuance |
| Data | Trusted freeze/export reads actual observations and canonical replay; immutable native input receipts own lineage | Shared actual decoder and finalized-request inspector; canonical CEX worker | Exact Features/FutureMarks ResearchRow reconstruction, Replay hash and withheld-role boundaries | Complete freeze/finalize/execute and platform AttemptContext/artifact output wiring require final acceptance |
| Run | Producer constructs exact Run/Task from source budget, verified Build and verified development collection | PG Run registration and distinct CexCampaign launch path | Source reservation is authentic; software/data opaque objects remain separate | New kind/input registry must reject generic Train/Backtest relabeling and receive the exact data-bound source export |
| Session | Host-owned native app-server and operator stdin; private native state and broker scopes own access | Research namespace API; PG completion delivery ledger | Existing source transport tests plus separate paused-platform/checkpoint PR evidence | Real native/provider, PVC restore and notification delivery are deployment/readback claims; synthetic peers do not prove them |
| Results | Trusted controller reads immutable objects and provider state; worker output is evidence, not authority | Artifact gateway readback and PG result/outbox publication | Existing generic result hash/receipt checks; source budget never refunds unknown output | CexCampaign summary must bind native request, collection, model/replay/evaluation receipts and stopped execution |
| Settlement | Independent native host consumes task, Job, Pod, receipt and stop readback | Native cumulative family/Study settlement under source guards | PlatformTransferred excludes the old claim and legacy settlement paths; unknown consumption retains charge | Dedicated platform-to-native terminal bridge is not implemented by prepare-platform or a worker hash |

Research subfunctions cannot acquire trading authority from a Run, Build, Session or budget witness.
CEX and Prediction remain market-family research modules behind the shared runtime/risk/execution seams.
This producer imports no execution adapter and exposes no order, risk-limit or runtime-resume operation.
