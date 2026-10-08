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
| Expiry and revocation | OSS checks expiry per request; PG locks current exact independent approval through Build registration | Independent approval PG owner; native Run revocation remains separate |
| Scientific task lease/fence/cancel | Existing PG admission, Gateway and AttemptWriter remain | Existing runtime acceptance; no change here |
| Promotion, holdout and trading | Existing separate governance/runtime rules | Build publication/import grants none of these |

The shared Release orchestration accepts required-check conclusions of `success`
or `skipped`. Research issuance and ACK import independently require all three
authenticated checks to be `success` (latest exact-source checks for issuance,
signed original check IDs for ACK); a skipped check therefore blocks
research release acceptance even when the shared orchestration succeeds. Keep
the independent `acr-publish.yml` publisher workflow/run/attempt/job identity;
the Release orchestrator is not its issuer. Do not relax native checks to make
the shared orchestration's broader admission imply research acceptance.

## Public publisher configuration

Keep `MONDAY_RESEARCH_RELEASE_POLICY` and `MONDAY_RESEARCH_RELEASE_SIGNING_KEY`.
Remove the ordinary workflow's Gateway, Broker and optional PG import settings.
Retain existing ACR configuration and publisher job `id-token:write`.
The issuer compiles before any private signing credential enters its environment.

For CI, add `oss_by_product` to the repository public policy, keyed by `cex-runner`, `prediction-runner` and/or `controller`. Each selected product needs its own independently approved existing RAM role and exact source/Build prefix list. CI projects only that product’s entry into the native PublisherPolicy `oss` field using `select-research-oss-policy.jq`. Missing products, unknown keys and shared role ARNs fail closed; no union role scope is used. ACK receives a separately pinned native policy with a single `oss` entry.

Each OSS entry contains:

```text
bucket: actual existing bucket
region: actual OSS region, for example cn-hangzhou
endpoint: https://BUCKET.oss-REGION.aliyuncs.com/
role_arn: approved CI publication RAM role ARN
oidc_provider_arn: approved GitHub RAM OIDC provider ARN
audience: operator-selected registered GitHub/RAM audience
subject: exact operator-approved GitHub OIDC sub (including any customization)
role_prefixes: sorted exact source/Build prefixes already enforced by the RAM base role
repository_id: verified immutable GitHub repository ID
owner_id: verified immutable GitHub owner ID
```

### Migrate existing configuration without recreating resources

Keep three facts separate: an existing Alibaba bucket/repository/role, a missing
GitHub public policy mapping, and actual role permissions not yet independently
verified. `MONDAY_RESEARCH_RELEASE_POLICY lacks oss_by_product mapping` is a
local configuration error before RAM/OSS/ACR calls. It does not establish missing
cloud resources. The prior Gateway publisher policy had signing trust, builder
and image repositories but no per-product OSS identity mapping; those existing
settings and resources remain reusable. Do not rebuild them to fix this error.

Read the existing public policy through the approved operator surface and ask
the read-only inventory owner to compare existing role trust/actions/resources
with the exact native scope plan. Prepare a public role-map JSON object keyed by
the selected products, with the complete fields above, using only verified
existing values. Each product must retain its independently reviewed distinct
role and exact sorted prefixes. Whether existing roles qualify, or any specific
IAM delta is needed, is an inventory result, not inferred from this error.

```bash
bash .github/scripts/migrate-research-oss-policy.sh \
  EXISTING_PUBLIC_POLICY_JSON REVIEWED_EXISTING_ROLE_MAP_JSON > CANDIDATE_POLICY_JSON
```

This offline command preserves every existing policy field and only adds the
explicit mapping. It rejects unknown products or fields, shared roles, wildcard
prefixes, missing fields, unbound endpoints and duplicate JSON documents.
An identical repeat is idempotent. A new product can be added only when every
previously approved entry is included unchanged. Removal or replacement fails.
Its structural checks do not prove actual RAM permissions, endpoint trust or
native signing policy acceptance. It performs no API calls, key creation, role
creation, IAM writes or repository-variable updates. Review the public JSON diff
before the separately authorized variable update; review only demonstrated IAM
differences before any permission write. Never obtain values from collector
credentials or fill unknown role/provider/audience/subject values by guessing.

For example, a controller-only migration supplies `{ "controller": OSS_ENTRY }`;
publishing all three products needs all three independently reviewed entries.
A single legacy role cannot be silently copied into all products. The selected
product projection still goes through the same native validation and real IAM
negative acceptance required below. A presence check only confirms the variable
and signing secret are set; it cannot establish that this migration is complete.

Resolve IDs through authenticated GitHub reads. Do not guess IDs or create credentials.
The ACK policy can use the same bucket with its internal HTTPS endpoint.
The code accepts only bucket/region-bound Alibaba endpoints, without redirects or ambient proxies.
It does not require GitHub native attestation or ACR OCI 1.1 referrers.

`research-release-capability oss-source POLICY CONTEXT - SESSION_FILE` exchanges job OIDC with RAM STS.
It requests read access to only `research/sources/COMMIT/` before ACR login.
`oss-publish POLICY CONTEXT PLAN SESSION_FILE` requests the native plan's exact sorted source/Build prefixes.
The requested prefixes must be a subset of the configured approved role prefixes; publication must equal that complete list. The session policy intersects with the operator role and grants no deletes, ACL changes, listing or scientific output writes. This client configuration is a consistency check, not proof of the actual RAM policy.
Both phases also need `oss:GetBucketVersioning` on this exact bucket.
The client checks exact issuer/audience/subject, repository/owner IDs, main, workflow, source, run, attempt and the job's authenticated check-run URL. It also requires STS to return the same verified OIDC subject/issuer/audience.
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
The issuer rechecks current main, original required-check IDs and the active exact publisher before signing and after both signed objects have been uploaded and independently read back. A failed final check leaves evidence objects but fails publication; it cannot recall a signature already written.
ACK requires that the complete publisher run and exact job finished successfully before importing.
Cancelled, incomplete, failed or wrong-workflow original attempts cannot import through this path. A later successful rerun does not substitute for the signed original attempt.
A residual object write provides no PG, scientific Run, promotion or trading authority.
Reimport of the same exact valid release is idempotent in PG; a foreign proof/OCI/Build is rejected.
This does not consume a scientific admission nonce or create another Run.

Actual IAM remains an independently approved deployment prerequisite:

1. RAM must trust the GitHub issuer, registered audience and exact repository/main/publisher workflow subject.
2. Bind immutable IDs where the available RAM conditions support them.
3. Verify the actual subject customization and supported claims before enabling publication.
4. The base CI role itself must restrict read/create to the exact preapproved native source/Build prefixes in the existing bucket. A broad `research/sources/*` or `research/builds/*` role with only caller-supplied inline Policy is insufficient.
5. The public `role_prefixes` list must match those trusted base-role resources. Updating that per-release scope is a separately approved IAM operation, not performed by CI or this branch. This initial model does not supply a low-interaction RSI publication loop: every new release needs independently approved scope or a separately reviewed trusted updater. An isolated fixed release namespace would be a different authorization model, requiring explicit parent approval and real overwrite/delete/negative tests; it is not implemented or treated as equivalent here. The caller-supplied session Policy adds restriction; it is not the trusted scope boundary.
6. Deny delete, ACL changes, bucket mutations and overwrite-header bypass through the reviewed bucket/role policy.
7. ACK gets separate read-only OSS permissions and the dedicated PG importer role.
8. Promotion signing and scientific AttemptWriter credentials remain separate.

An untrusted workflow can request its own STS policy if the RAM trust permits it.
Client validation alone cannot constrain such a caller. **Deployment is blocked** until real RAM tests show that sessions obtained with omitted or expanded Policy still reject object access outside the base role’s exact prefixes; tests must also reject another source/Build prefix, wrong/missing subject, wrong workflow and repository IDs, replay after expiry, delete and overwrite-header bypass. RAM may validly allow an exchange without Policy; the resulting base-role permissions must remain narrowly scoped. No offline fixture or configuration list establishes these denials. If the current RAM integration cannot enforce the exact dynamic scope independently, retain publication blocked and review a supported authorization alternative.
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
If the existing bucket is versioned, this initial publisher remains blocked. This is a constraint of this adapter, not a claim that OSS cannot safely publish versioned evidence.
Choosing another existing compliant bucket or a version-preserving backend needs a separate reviewed decision. A future version-preserving publisher must capture the exact PUT version ID, bind it to signed evidence and independently GET that version with digest/size verification. That backend is not implemented in this PR; never disable or suspend existing versioning to enable this adapter.
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

The `controller` product now builds `research-release-publisher` with its owning platform workspace, locked release profile and `publisher` feature. The existing compiler-input manifest records that recipe, lock/profile bytes and compiler environment; producer artifacts and controller OCI verification include its executable digest and exact readback. The controller image installs git, gh, Ruby, unzip and CA certificates and verifies these dependencies plus the source-bound importer CLI offline. Neither probe invokes the campaign entrypoint or supplies credentials.

Use that authenticated release binary for the existing controlled ACK execution image; never install the separate temporary CI debug issuer. The actual existing control capacity and its operator execution surface must still be read back before deployment. The controller artifact is a packaging source, not permission to replace a different control service or start a Campaign.
The foundation Dockerfile requires that verified binary in `research-control-bin`; stage it only from the authenticated controller artifact after checking the exact source/compiler/producer/digest. Its reviewed runtime base must independently supply git, gh, Ruby, unzip and CA. Merely building that Dockerfile or copying a host binary does not establish those prerequisites.
Run the importer through the existing operator execution surface in that Pod or host.
Do not add a release Gateway/Broker Pod, service, ingress or PVC.
Do not mount signing keys or collector credentials there.
The current foundation NetworkPolicy does not allow GitHub or OSS egress.
An operator must review exact existing egress destinations and TLS trust before this command can work.
This branch does not open network access or change that policy.
This command does not start a research provider, migrate PG or enable an authority.

The importer uses `gh api --paginate` and parses every authenticated JSON page
locally, including downloads through `jq -s`. It does not require the newer
`gh --slurp` flag: the existing distro CLI can be reused. Malformed/truncated
pages, API failure and missing/ambiguous original identities still fail closed;
this compatibility change does not weaken producer or signature verification.

The host must supply these separate inputs:

- Public policy and signed release selectors from the exact completed CI publication.
- An existing authenticated Git checkout for GitHub reads; `gh` must be installed.
- A short-lived read-only OSS session in a private 0600 regular file.
- A short-lived read-only private-repository GitHub token, available as `GH_TOKEN` to `gh`.
- `MONDAY_RESEARCH_DATABASE_URL` for the existing dedicated Build importer with verified PG TLS.
- A private signed admission envelope, atomically replaced by a separate approved authority owner, and its independently installed current PG state.
- Operator-pinned `import_admission_keys` public keys in a read-only host policy mount. These keys must differ from all CI release keys; the publisher and importer must have no write access to the policy or admission mount.

The independent ACK envelope has `schema:2`, `key_id`, `admission` and canonical lowercase `signature_hex`. Admission fields are `schema:2`, positive signed `revision`, `expires_ms`, `build_sha256`, `image_sha256`, `publication_proof_sha256`, and `revoked`. Its signature domain is `monday.ack-build-import-admission.v2`; this changes only the independent approval, not CI release signatures. Schema 1 approvals fail closed. Revision orders independently signed instructions; it is not a wall-clock signing-time proof. Use one independent writer per selector tuple, serialize signing/installing, and prohibit pre-signing future approvals or reusing a revision. The owner must track every issued revision, including signed-but-not-yet-installed instructions, and issue revocation above all of them; reading only the installed PG tip is insufficient if future approvals were pre-signed. The owner reads the current/issued revision and signs a strictly greater one for any approval change or revocation; identical same-revision retry is idempotent. PG prohibits selector-key changes, revision rollback and different content at an existing revision. Even an older valid signature never previously installed cannot reactivate a newer revocation. Reapproval requires a freshly signed higher revision, not restoring an old envelope. Sign `SignedImportAdmission::signing_bytes()` using the separately controlled operator Ed25519 key; the importer has only its public key. A private 0600 file alone is not authority. Unsigned files, CI-key signatures, key reuse and tampering are rejected. This branch does not create or copy that operator private key.
On the separately controlled operator host, use the verified binary and **existing approved** private key. Prepare an unsigned admission JSON with the seven exact fields listed above, then execute:

```bash
umask 077
research-release-publisher sign-import-admission POLICY_FILE ADMISSION_JSON OPERATOR_KEY_ID EXISTING_PRIVATE_KEY_FILE > SIGNED_ADMISSION_NEW
```

The command checks the pinned independent public key, rejects CI-key reuse or a foreign key, and emits only the signed envelope; it never creates keys or grants cloud/Run authority. It can sign `revoked:true` updates for the same exact selectors. The operator atomically installs that envelope at the independently owned read-only ACK mount through the already approved control surface. Do not mount this private key into CI, the importer or research jobs. Key provisioning, policy/mount writes and approval lifecycle remain separate action-time approvals; producing an envelope does not authorize their installation.

After separately approving the schema/role change, the independent owner uses its existing PG login (membership only in `monday_research_build_import_owner`) to install or revoke the signed envelope:

```bash
research-release-publisher set-import-admission POLICY_FILE SIGNED_ADMISSION_NEW
```

This command verifies the independent signature and exact selectors before PG UPSERT and independently reads back the installed document/hash. UPSERT commits before the independent readback; a later readback error does not prove rollback. On error, the owner must inspect current state and immutable audit, reconcile concurrent owner actions, and never blindly retry an older approval over a later revocation. A signed `revoked:true` update targets the same selector row. Reapproval is an explicit independently owned state change, not restoring a host file. No owner PG credential is mounted in the importer.
Native registration explicitly calls the mandatory `lock_build_import_admission` within its transaction; an old schema missing that function fails closed before any Build write, independently of trigger installation.
For fresh schema installation, apply `sql/build_import_admission.sql` after `verified_build_release.sql`, before foundation `postgres/roles.sql`. For an already installed schema, the action-time reviewed additive diff is that migration, a new NOLOGIN `monday_research_build_import_owner`, schema USAGE plus SELECT/INSERT/UPDATE on `build_import_admissions` and SELECT-only on its audit for that owner; the existing importer gets SELECT-only on approval state and EXECUTE only on `research.lock_build_import_admission(text,text,text,text)`, and never owner membership. The trigger is SECURITY DEFINER, pinned to `pg_catalog`, owned by the trusted schema owner; PUBLIC has no table/function permissions. Owner/importer receive no DELETE, schema ownership or trigger alteration. Install migration/upgrade grants atomically with a trusted schema owner (for example `psql --single-transaction --set ON_ERROR_STOP=1`); independently read back functions/triggers/owners/grants before enabling the login. Do not re-run the fresh-install roles file against existing roles. No migration or grants have been applied to real PG.

No wildcard or omitted selector is accepted. Expired or revoked approval fails before readback.
The importer rereads and verifies this envelope after object/GitHub checks, immediately before PG registration. The signed file is necessary but insufficient. `build_import_admissions` stores the current envelope hash, expiry and revocation under the stable `(Build, OCI, proof)` key with monotonic signed revision. Restoring an older valid signed file cannot replace this independent PG state. Read-only independently owned policy mounts remain mandatory; replacing public trust or obtaining the independent PG owner identity is outside the importer authority boundary.
Issuance requires current main before signing and after signed-object readback. Import instead reads the signed original successful run/attempt/job and each original authenticated check ID directly. Later main commits, newer checks or later attempts do not invalidate those immutable release identities. Historical import or rollback still needs a current independent signed admission for the exact Build/OCI/proof, a matching separately approved read-only OSS scope, valid public trust and complete readback. Missing/deleted/failed original GitHub records fail closed; no latest-check or unsigned fallback is accepted.
The registration transaction's database trigger takes `FOR SHARE` on the current approval and requires its exact envelope hash, selectors, future expiry and unrevoked state. This lock lives through commit; the independent owner's approval/revocation `UPDATE` conflicts with it. Revocation-first rejects import; import-first makes revocation wait. When revocation returns successfully, a later registration (including idempotent retry) cannot use the old approval. Already imported immutable Builds remain readable and no Run is created. Every owner update appends immutable audit history.
Expiry's mandatory boundary is the serialized insertion check, not an impossible wall-clock guarantee after COMMIT. The native default transaction also has a deferred expiry recheck; SQL clients can move that constraint's timing with `SET CONSTRAINTS IMMEDIATE`, so this is additional protection, not an unchangeable SQL commit-time expiry contract. Constraint timing cannot bypass the mandatory BEFORE trigger or its revocation lock. Existing grants for running science still have their independent native deadline/fence/revocation checks.
All CLI registration goes through `oss-import` and the original GitHub producer/check verification. The obsolete Gateway release `import` CLI and public shortcut helper were removed because they lacked that original completion check. Runtime Gateway/AttemptWriter and HTTPS readback fixtures remain. The PG trigger rejects direct SQL without current exact independent approval; it does not verify executable bytes or Ed25519 in SQL. The importer login and verified executable therefore remain entrusted only to the controlled host, never arbitrary Agent tools. Original source/compiler/OCI/readback checks remain native and mandatory.
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
3. Independently approve/install the additive PG admission migration/roles; verify real grant isolation, stable approval replay rejection and both revocation lock orders. Verify the complete real negative IAM list above, independently owned admission/policy mounts, cancellation/expiry and signature checks. Keep deployment blocked without this evidence.
4. Publish one exact test release through GitHub/ACR/OSS; retain run/attempt/job and all digests.
5. Independently import it from existing ACK capacity; verify real OSS bytes, signature and PG projection.
6. Keep research paused until its separate runtime admission/readback succeeds.

On failure, stop research publication/import and retain all evidence.
Restore prior publication workflow only if its already-approved Gateway/Broker exists; otherwise keep publication blocked. Retain the additive approval migration and audit history. An older importer cannot supply the new mandatory approval context, so it fails closed; never remove the approval trigger or grant broad rights as rollback.
Do not delete objects, reverse migrations, reset immutable PG records or restart collectors.
No new persistent infrastructure needs removal.
Real GitHub/RAM/OSS/ACR/ACK acceptance has not been performed by offline tests.

## Controlled issuance and RSI boundary

The executable first-release sequence is: consume the final-main authenticated compiler artifact; invoke the read-only native `scope-plan` with its real completed software run and public policy to obtain exact source/Build prefixes before any publisher/STSes exist; present the base-role resources and unchanged trust/actions as a concrete IAM diff to the independent owner; approve and verify that diff before CI OIDC exchange; publish one controller release; sign and install its independent PG approval; import with separate read-only ACK identity and read back the immutable Build. These commands exist; unknown native plan/digest/role values must be filled from actual final-main artifacts rather than PR fixtures. No IAM write is performed by these tools.
On the existing independent operator host, use the authenticated final-main compiler artifact's verified native CLI (not a guessed host build) and a clean exact source checkout:

```bash
research-release-publisher scope-plan SOURCE_ROOT FINAL_MAIN_SHA SOFTWARE_RUN_ID \
  ACTUAL_SOFTWARE_PRODUCTS controller PUBLIC_POLICY_FILE > RELEASE_SCOPE_PLAN
```

The software product list is normalized by the existing catalog and must match the actual producer manifest exactly. The command verifies current main and three authentic required gates before and after downloading real producer bytes, the completed original software run/attempt/job, target/locks/builder/product bindings and exact source archive; it shares Build projection with issuance `plan`. Tracked source/script changes fail. Output contains source, BuildSpecs and sorted exact publisher prefixes; it contains no fabricated OCI digest, future publisher identity, signature, session or grant. It needs read-only GitHub access only and neither requires configured/approved OSS role prefixes nor exchanges STS. Actual issuer `plan` still requires active publisher authority and checks its real OCI identity elsewhere. A manual rebuild's completed software job may be consumed during that active publisher workflow; independent pre-approval `scope-plan` instead requires its entire software producer run to be completed successfully.
The owner approves only this independently computed exact prefix delta after reading actual existing base-role actions/trust/resources. Update the public product policy to the approved exact prefixes and verify real denial cases, then execute the ordinary authorized publication. This closes the planning bootstrap without letting CI self-approve IAM. The output is approval material, not proof that RAM has those permissions. A new source or compiler Build invalidates this planning input and requires a new exact plan/approval.

A scientific loop can reuse an already verified Build under distinct native task grants, leases, budgets and AttemptWriter scopes; it does not require new release IAM scope for each candidate. A loop that edits software and creates a new Build needs a new independently approved exact prefix. Automatic new-Build RSI publication is therefore not implemented or accepted as complete. Its supported next implementation must put a trusted updater on an existing approved operator execution surface, verify final-main/native plan independently, obtain an explicit bounded release grant, apply only that exact role-policy delta and journal/read back it. The updater must not share CI/Agent authority, cannot mint grants, and must prove expiry/cancel/replay/negative IAM behavior before enablement. This requires a separately reviewed authorization contract and concrete credential/policy approval; neither a broad wildcard base role nor CI-authored session Policy can substitute.
