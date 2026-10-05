# Native request revocation receiver

A source revocation can become effective after the host exports its witness.
For example, a 15:00 export can contain a 16:00 revocation. The request remains
active until 16:00. It cannot launch work whose deadline exceeds 16:00.

The receiver uses `monday.native_request_revocation.v1`. It binds tenant,
request SHA, original operation SHA, family, root grant, source receipt SHA,
source effective time and host issued time. Hash the original structured
operation ID with canonical JSON. Do not hash its raw string bytes instead.
The reason SHA identifies the complete authenticated, published source object.
The source producer must authenticate that object and independently read it back.
A host signature cannot replace those checks.

`NativeAdmissionTrust::verify_revocation` returns an opaque verified witness.
It uses the native reservation public-key trust and strict Ed25519 verification.
Software release keys cannot become native reservation issuers.
Both native admission and revocation messages sign a JSON tuple containing
domain, key ID and evidence SHA. A trusted alias cannot replace the signed key ID.
Old signatures remain read-only audit evidence. They cannot launch work through
the current receiver. The source producer must sign a fresh projection.

The operator calls `researchctl register-native-request-revocation SIGNED_FILE`.
Set `MONDAY_RESEARCH_NATIVE_ADMISSION_TRUST_FILE` to the reviewed public trust file.
The receiver first verifies the signature. PG then locks the original admission.
It verifies the stored native import and matches tenant, request, operation,
family and root grant. Unknown or changed identities fail before insertion.
Neither the command nor the import activates PG, refunds budget or settles work.

PG appends each source cause to `research.native_request_revocations`.
An exact signed retransmission is idempotent. Conflicting evidence for the same
request and source receipt fails. Multiple source causes retain their own effective
times. The earliest effective time wins, even when that cause arrives later.
Existing immediate manual rows in `research.revocations` remain unchanged.

`research.native_request_deadline_ms(request)` returns the minimum of native
expiry and all imported source effective times. A missing native import returns
NULL. This read-only cap does not grant admission. Callers must also verify
current admission, manual revocations, tenant and their own lease or fence.
Consumers order writes by locking the original admission row first.
The reconciler caps the task deadline and cancels a request when its cap passes.
Prepare checks, upload permits and terminal publication reject expired requests.
The upload function retains its original signature and authority/task/admission
locks. Source import waits until an admitted upload releases that lock.

Install `native_request_revocation.sql` after `native_admission.sql`.
The earlier migration remains an immutable snapshot. The new migration creates
an append-only table and a fixed read-only deadline function. Installation starts
from the existing authority state and never changes it. No production migration
or signing configuration is included in this change.

The trusted importer needs SELECT on admissions and native imports, plus
SELECT/INSERT on native request revocations. PG row locking also requires UPDATE
privilege on an admission column. The immutable admission trigger still rejects
actual UPDATE statements. Keep this role separate from Agents and workers.
Consumers need EXECUTE on `research.native_request_deadline_ms(text)`.
The gateway keeps EXECUTE on the existing artifact permit function.
Neither function grants direct writes or access to signed witness documents.

Tests use synthetic signatures and a disposable loopback PG database.
They verify identities, append-only history, future timing, locks and role limits.
They do not prove real source issuance, cloud execution or scientific completion.
Production source export, prepared collection, canonical workload execution and
independent settlement still require their own authenticated producer and readback.
