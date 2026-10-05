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

Public budget inspection, exclusive transfer and guarded export always sample the real clock after acquiring approval, Study and family guards. Callers cannot supply historical time. Internal source-ledger tests inject clocks to verify expiry and serialization; those helpers are not public APIs.

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
The main receiver verifies purpose-bound signatures and preserves scheduled effective times.
Actual source-to-PG publication and import remain independent readback steps.
This interface does not issue or revoke arbitrary row-wide approvals.

Platform terminal settlement still requires independent task, Job, Pod, receipt and stopped-process evidence.
A worker hash or claimed success cannot refund or settle the native reservation.
The legacy dispatch and settlement paths remain excluded after platform transfer.
That complete terminal bridge is not implemented by the source exporter.

`mission dispatch export-platform` adds a controlled code path after preparation.
It verifies the release signature and independently streams the exact source/archive and executable bytes.
It obtains the CEX opaque proof from actual collection and block readback, using the durable reservation's request hash.
It independently reads an existing immutable Kubernetes configuration Secret.
The static contents must be exactly `campaign.json`, `artifact-io.json`, `ca.pem`, and `native-trust.json`.
The request bytes match finalization; public native trust matches the controlled host trust.
The shared `worker_configuration_reference` identity binds namespace, Secret name, UID and sorted encoded static contents.
The Run binds the entire verified collection; its Task copies the inspected native worker command and changes only the fixed request mount to `/config/campaign.json`.
CPU, memory, target, source, image, ABI and the original full Job duration must match the source witness.

Pure publication URL and public signer-role checks precede exclusive transfer.
An invalid operation or bucket address cannot transfer the already reserved native budget.
The host then transfers the existing native operation and publishes/reads back its transfer receipt before signing.
It then reacquires the source guards and loads a distinct private witness key from a regular 32-byte file.
The key file uses mode 0600 and a canonical private mode-0700 parent; FIFO and symlink inputs fail closed.
The signer cannot reuse registered Root/Study authority or software-release public keys.
Retained admissions and revocations repeat the same public role check without loading a private key.
A valid old signature cannot bypass role separation or rewrite its original issue time.
Current native admission signatures bind the JSON tuple `(domain, key_id, evidence_sha256)`.
The prebuilt CLI neither starts Cargo nor exposes a raw statement-signing RPC.
Signed exports are create-once and independently read back; ambiguous publication or import retains the full source charge.
The source CLI leaves controlled PG registration and terminal settlement to their explicit consumers.

Attempt credentials remain outside the signed static configuration to avoid a task/token identity cycle.
The worker uses `/identity/artifact.token` and `/identity/tls.pem`, mounted through controlled private staging.
The existing gateway projection supports exact tenant/task/attempt/fence writers and independently checks PG upload permits.
The main controller issues late identities only for the imported native Task and current Attempt.
It verifies the original launch context, static Secret UID/bytes and native trust before launch.
The exporter creates no writer token or Kubernetes resource.
This code path and local tests do not prove a live Secret, broker projection, cloud import, launch or scientific outcome.

Tests use synthetic native grants, published receipt peers and local source ledgers.
They verify debit retention, ownership, expiration, revocation and callback ordering.
They do not prove cloud execution, real native budget issuance or scientific completion.

## Functional ownership and completion review

This review separates source authority, executable publication and actual scientific consumption.
The main baseline is `add8cb169950de9817c7642bae9f3259418d956a`.
It includes native admission/revocation, controlled Attempt identities, registered Session recovery and mechanical retirement.
The input/worker dependency is private CEX candidate `bbb36ff676268348d05ab6f8aff2f4e62c5d9d69`; it is not a main merge.
The original published producer source `3d53c571dd7c7dd92691481c879e119a7013dafe` remains frozen.
Actual publication, import, launch, provider recovery and scientific outcomes have not been performed by this integration.

| Subfunction | Caller and authority owner | Actual consumer | Verified boundary | Remaining gap |
| --- | --- | --- | --- | --- |
| Publication | Native software issuer and release verifier; software keys and registry policy own release authority | Verified Build registration; reconciler executable readback; released worker executable | Separate source/build/image/executable identity and signed release checks | Actual publication/registry readback must be recorded per immutable release; it proves no budget or scientific result |
| Admission | Controlled native importer; original native Root, approval, family and Study remain scientific authority | PG submit, launch, preparation permit, upload permit and terminal commit | Signature/tenant/request/operation/expiry bindings; unsigned and reused-operation rejection | Complete source producer and data-bound import are still being connected |
| Native budget | Canonical finalized dispatch inspection; native source ledger owns cumulative debit and ownership | prepare-platform; guarded opaque budget/export callbacks | Published authentic receipts, full charge retention, original deadlines, exact transfer and revocation ordering | Exact signed Run/Task/data export and controlled PG import remain pending |
| Revocation | Native Root/Study revocation receipts; host witness projects only an existing transfer | Source export guards; future effective-time PG launch/prepare/upload/terminal guards | Published reason objects and scheduled times; separate Root/Study causes preserve earlier constraints | The receiver is merged Code; actual signed source publication/import remains separate. Request revocation cannot issue arbitrary approvals |
| Data | Trusted freeze/export reads actual observations and canonical replay; immutable native input receipts own lineage | Shared actual decoder and finalized-request inspector; canonical CEX worker | Exact Features/FutureMarks ResearchRow reconstruction, Replay hash and withheld-role boundaries | Complete freeze/finalize/execute and platform AttemptContext/artifact output wiring require final acceptance |
| Run | Producer constructs exact Run/Task from source budget, verified Build and verified development collection | PG Run registration and distinct CexCampaign launch path | Source reservation is authentic; software/data opaque objects remain separate | The merged distinct CexCampaign registry rejects generic Train/Backtest relabeling. Production source export still requires the complete data dependency |
| Session | Host-owned native app-server and operator stdin; private native state and broker scopes own access | Research namespace API; PG completion delivery ledger | Existing source transport tests plus separate paused-platform/checkpoint PR evidence | Real native/provider, PVC restore and notification delivery are deployment/readback claims; synthetic peers do not prove them |
| Results | Trusted controller reads immutable objects and provider state; worker output is evidence, not authority | Artifact gateway readback and PG result/outbox publication | Existing generic result hash/receipt checks; source budget never refunds unknown output | The typed CexCampaign result and full original scientific readback remain distinct; a compute receipt alone cannot prove science |
| Settlement | Independent native host consumes task, Job, Pod, receipt and stop readback | Native cumulative family/Study settlement under source guards | PlatformTransferred excludes the old claim and legacy settlement paths; unknown consumption retains charge | Independent Source terminal audit remains a separate delivery. Its mechanical full charge cannot refund or promote science; signed published coverage precedes retirement |

Research subfunctions cannot acquire trading authority from a Run, Build, Session or budget witness.
CEX and Prediction remain market-family research modules behind the shared runtime/risk/execution seams.
This producer imports no execution adapter and exposes no order, risk-limit or runtime-resume operation.
