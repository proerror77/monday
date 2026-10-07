# CI release to OSS; independent ACK import

This is code and offline acceptance. No IAM, credentials, resource, Pod or deployment was changed.
The collector release remains separate. Ordinary research publication needs no release Gateway or Broker service.
The existing control Pod retains its runtime Gateway and AttemptWriter.

## Responsibilities and guarantees

| Guarantee | Owner and implementation | Deployment prerequisite |
| --- | --- | --- |
| Exact current-main source and three authenticated checks | Native CI issuer; rechecks after object readbacks | Read-only GitHub API token |
| Original software workflow, run, attempt and job | Existing authenticated compiler artifact downloader | Unexpired original Actions artifact |
| Actual compiler, locks, recipes and OCI program bytes | Existing native plan and publisher, unchanged | Digest-pinned builder and ACR pull |
| Release signature | CI Ed25519 key; existing receipt format | Approved signing secret and operator public trust |
| No same-key replacement | OSS V4 PUT with signed `x-oss-forbid-overwrite:true`; check bucket versioning before each PUT | Existing never-versioned bucket; approved policy must deny bypass writes/deletes |
| Independent bytes | Separate bounded OSS GET after PUT; ACK repeats source/program/proof checks | Separate ACK read-only identity |
| Build projection | ACK importer verifies signature, exact selectors and completed producers; immutable PG transaction and readback | Existing dedicated Build importer role |
| Expiry and revocation | Every OSS request checks session expiry; ACK rereads host admission before registration | Host-owned admission updater; separate scientific revocation contract |
| Scientific task lease/fence/cancel | Existing PG admission, Gateway and AttemptWriter remain | Existing runtime acceptance; no change here |
| Promotion, holdout and trading | Existing separate governance/runtime rules | Build publication/import grants none of these |

## Public publisher configuration

Keep `MONDAY_RESEARCH_RELEASE_POLICY` and `MONDAY_RESEARCH_RELEASE_SIGNING_KEY`.
Remove the ordinary workflow's Gateway, Broker and optional PG import settings.
Retain existing ACR configuration and publisher job `id-token:write`.
The issuer compiles before any private signing credential enters its environment.

Add `oss` to the existing public PublisherPolicy:

```text
bucket: actual existing bucket
region: actual OSS region, for example cn-hangzhou
endpoint: https://BUCKET.oss-REGION.aliyuncs.com/
role_arn: approved CI publication RAM role ARN
oidc_provider_arn: approved GitHub RAM OIDC provider ARN
audience: operator-selected registered GitHub/RAM audience
repository_id: verified immutable GitHub repository ID
owner_id: verified immutable GitHub owner ID
```

Resolve IDs through authenticated GitHub reads. Do not guess IDs or create credentials.
The ACK policy can use the same bucket with its internal HTTPS endpoint.
The code accepts only bucket/region-bound Alibaba endpoints, without redirects or ambient proxies.
It does not require GitHub native attestation or ACR OCI 1.1 referrers.

`research-release-capability oss-source POLICY CONTEXT - SESSION_FILE` exchanges job OIDC with RAM STS.
It requests read access to only `research/sources/COMMIT/` before ACR login.
`oss-publish POLICY CONTEXT PLAN SESSION_FILE` requests the native plan's exact sorted source/Build prefixes.
The session policy intersects with the operator role and grants no deletes, ACL changes, listing or scientific output writes.
Both phases also need `oss:GetBucketVersioning` on this exact bucket.
The client checks repository/owner IDs, main, workflow, source, run, attempt and the job's authenticated check-run URL.
RAM, rather than the client, verifies the OIDC signature.
Session files require a canonical 0700 parent, new 0600 regular file, and exit cleanup.
Session policy length is bounded by RAM's 2,048-character limit.

## Authorization differences and cloud approval

RAM supports a minimum 900-second STS session. This path requests that minimum and never renews it.
Publication that exceeds this window fails closed; it cannot silently refresh authority.
The old Broker granted at most two minutes, renewed about every minute, and removed storage access after cancellation or main drift.
STS does not reproduce that behavior. A cancelled job's already-issued storage session can remain usable until expiry.
A stolen OIDC token can also be replayed to RAM while RAM still accepts it.
This code does not claim single-use OIDC, immediate IAM revocation or immutable storage against a privileged bucket owner.

The replacement boundary is complete release acceptance, rather than every storage write.
The issuer rechecks current main, required checks and the active exact publisher before signing.
ACK requires that the complete publisher run and exact job finished successfully before importing.
Cancelled, incomplete, failed, wrong-workflow or superseded attempts cannot import through this path.
A residual object write provides no PG, scientific Run, promotion or trading authority.
Reimport of the same exact valid release is idempotent in PG; a foreign proof/OCI/Build is rejected.
This does not consume a scientific admission nonce or create another Run.

Actual IAM remains an independently approved deployment prerequisite:

1. RAM must trust the GitHub issuer, registered audience and exact repository/main/publisher workflow subject.
2. Bind immutable IDs where the available RAM conditions support them.
3. Verify the actual subject customization and supported claims before enabling publication.
4. The base CI role may read/create only release sources and Builds in the existing bucket.
5. Session policy narrows that role to the exact native plan prefixes.
6. Deny delete, ACL changes, bucket mutations and overwrite-header bypass through the reviewed bucket/role policy.
7. ACK gets separate read-only OSS permissions and the dedicated PG importer role.
8. Promotion signing and scientific AttemptWriter credentials remain separate.

An untrusted workflow can request its own STS policy if the RAM trust permits it.
Client validation alone cannot constrain such a caller. Do not enable this path before real RAM denial tests pass.
Do not copy a static account key into CI or reuse the collector credentials.
No IAM change is authorized by this code task.

## OSS identity and versioning

Source archives bind the exact Git commit and their deterministic archive digest.
Build program keys bind the Build ID; release proofs bind Build, OCI and publication proof digests.
Every accepted source/program is compared against the signed SHA-256 and byte count.
Release JSON is verified through the signature and proof identity, not ETag.
Uploads that conflict must still pass independent GET equality before signing.

OSS ignores overwrite refusal when versioning is Enabled or Suspended.
The adapter reads bucket versioning before each PUT and refuses those states.
It also checks this before ACR publication. Do not suspend versioning as a workaround.
If the existing bucket is versioned, this initial publisher remains blocked.
Choosing another existing compliant bucket or a version-preserving backend needs a separate reviewed decision.
A configuration change between versioning GET and PUT remains an IAM-controlled race.
Bucket mutation must be unavailable to CI and controlled through independent operator policy.

For reading historical versioned evidence, a host session may include `versions`, a map from exact object key to exact OSS version ID.
GET signs and encodes `versionId`; SHA-256 verification remains mandatory.
Without that map, GET reads the current object and requires its signed digest; mutation fails closed.
No code enables versioning, retention, WORM or an irreversible object lock.

References: [OSS V4 signing](https://www.alibabacloud.com/help/en/oss/developer-reference/recommend-to-use-signature-version-4),
[PutObject overwrite/versioning behavior](https://www.alibabacloud.com/help/en/oss/developer-reference/putobject),
[RAM AssumeRoleWithOIDC](https://www.alibabacloud.com/help/en/ram/developer-reference/api-sts-2015-04-01-assumerolewithoidc).

## ACK execution using existing capacity

Install the verified `research-release-publisher` binary into the existing controlled ACK execution image.
The foundation Dockerfile now requires that prebuilt binary in `research-control-bin`.
Run the importer through the existing operator execution surface in that Pod or host.
Do not add a release Gateway/Broker Pod, service, ingress or PVC.
Do not mount signing keys or collector credentials there.
The current foundation NetworkPolicy does not allow GitHub or OSS egress.
An operator must review exact existing egress destinations and TLS trust before this command can work.
This branch does not open network access or change that policy.
This command does not start a research provider, migrate PG or enable an authority.

The host must supply these separate inputs:

- Public policy and signed release selectors from the exact completed CI publication.
- An existing authenticated Git checkout for GitHub reads; `gh` must be installed.
- A short-lived read-only OSS session in a private 0600 regular file.
- A short-lived read-only private-repository GitHub token, available as `GH_TOKEN` to `gh`.
- `MONDAY_RESEARCH_DATABASE_URL` for the existing dedicated Build importer with verified PG TLS.
- A host-owned private admission JSON file, atomically replaced by the approved authority owner.

Admission fields are `schema:1`, `expires_ms`, `build_sha256`, `image_sha256`, `publication_proof_sha256`, and `revoked`.
No wildcard or omitted selector is accepted. Expired or revoked approval fails before readback.
The importer rereads this file after object/GitHub checks, immediately before PG registration.
It requires current main and the original authenticated required-check IDs.
Later main commits require a new publication; historical exceptions are not implicit.
Revocation that races after the final file read does not atomically revoke PG registration.
For that stronger boundary, use the separate PG scientific admission/revocation transaction before any Run.
Revoking storage publication does not remove previously imported Builds or invalidate existing Run grants automatically.

Execute within the existing controlled host:

```bash
research-release-publisher oss-import SOURCE_ROOT BUILD_SHA256 OCI_SHA256 PROOF_SHA256 \
  POLICY_FILE READONLY_SESSION_FILE PRIVATE_ADMISSION_FILE
```

The returned Build ID requires PG artifact readback. Build existence still does not authorize scientific compute.
The existing native research admission and task checks remain mandatory.

## Minimal migration and rollback

1. Review this branch and offline tests. Keep the collector production task untouched.
2. Independently approve and verify existing bucket state, RAM OIDC trust, scoped permissions and ACK identities.
3. Verify real negative IAM tests for wrong workflow, another prefix, deletes and overwrite bypass.
4. Publish one exact test release through GitHub/ACR/OSS; retain run/attempt/job and all digests.
5. Independently import it from existing ACK capacity; verify real OSS bytes, signature and PG projection.
6. Keep research paused until its separate runtime admission/readback succeeds.

On failure, stop research publication/import and retain all evidence.
Restore the prior workflow commit if its already-approved Gateway/Broker exists; otherwise keep publication blocked.
Do not delete objects, reverse migrations, reset immutable PG records or restart collectors.
No new persistent infrastructure needs removal.
Real GitHub/RAM/OSS/ACR/ACK acceptance has not been performed by offline tests.
