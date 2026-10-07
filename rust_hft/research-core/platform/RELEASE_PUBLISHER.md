# Independent Build release publisher

Ordinary CI uses direct OSS publication and independent ACK import.
See the [responsibilities, authorization limits, configuration and migration contract](../../../deployment/aliyun/research/foundation/RELEASE_OSS.md).
The HTTPS commands below remain for the existing gateway integration; ordinary CI no longer requires that service.

`research-release-publisher` is a separate native CI publisher and importer.
Research services only verify releases using operator public trust. They do not
load the issuer's private key. Issuance and import do not issue scientific grants,
charge budgets, submit tasks, enable providers, open holdout or activate runtime.

The `release-verification` feature exposes only existing public trust and opaque
`VerifiedBuildRelease` verification. It adds no issuer, PG or HTTP dependencies.
Signature verification does not prove independent source/program or OCI readback;
the actual producer still consumes those checks through its own Gate.

The native issuer accepts selectors, never claimed artifact hashes. It reads the
three authenticated GitHub Actions checks for exact current-main source. It
binds the original software workflow/run/attempt/job and the current ACR
publisher workflow/run/attempt/job. The job name must own the selected image.
The authenticated software artifact retains actual compiler/native/flags,
pinned builder container, locks, owner profile bytes and Cargo recipes.

Each image yields one Build per owning recipe and contained executable subset.
A Build never combines packages across workspace owners. Unrelated producer
binaries remain in the compilation proof, while each Build contains only its
image's programs. The ordinary BuildSpec still describes a valid scoped rebuild;
the publication proof additionally retains the producer's full input capture.

The issuer creates a deterministic `git archive` from exact committed source,
checks actual immutable OCI-contained programs against authenticated producer
bytes, publishes the source and programs, and independently streams their
HTTPS object readbacks. It publishes and reads back `release-proof.json`,
then rechecks mutable GitHub authority before Ed25519 signing. Immutable producer
verification is reused internally; store-boundary readbacks remain independent.
Signed release and BuildArtifact JSON are also published and read back.
Image-specific proofs live under `research/builds/{build}/releases/{oci}/{proof}/`.
Images can share the same compiled programs/Build without overwriting each
other's signed OCI binding. The proof identity also isolates authenticated
publisher attempts, so a new attempt cannot overwrite earlier evidence. Their common executable blobs keep the Build prefix.

`plan SOURCE_ROOT REQUEST POLICY` emits actual Build identities and exact release
prefixes without signing, uploading, importing or granting a Run. For the retained Gateway integration, the broker
must independently issue a capability scoped to these source/Build prefixes.
The code does not provision a broker or issue credentials.

`publish SOURCE_ROOT REQUEST POLICY PRIVATE_KEY_FILE HTTPS_GATEWAY TOKEN_FILE`
requires operator policy, a matching private 32-byte key encoded as 64 lowercase
hex characters in a 0600 regular NOFOLLOW file, and a scoped gateway token.
Absent or invalid configuration fails closed. No keys or sample production
credentials are included. Policy has this shape (all values below must be
supplied by the operator; do not use placeholder hashes or keys):

- `trust`: existing BuildReleaseTrust, with schema 1, repository,
  producer_workflow_path `.github/workflows/acr-publish.yml`, and public keys.
- `key_id`: a key in that public trust map.
- `builder_image`: the actual digest-pinned producer container.
- `image_repositories`: map of catalog product to exact allowed OCI repository.
- `tls`: optional private `ca_file` and combined PEM `identity_file` paths.
  TLS retains hostname verification and disables ambient proxies and redirects.

Before registry login, native preflight checks that policy admits the selected
GitHub repository, product, OCI repository and authenticated producer container.
It establishes HTTPS server trust with a HEAD request without a capability token.
A mismatched signer, policy, private TLS identity or server certificate blocks
registry publication.

CI compiles the native issuer in a separate step before injecting release
credentials. The wrapper only invokes that built binary. It does not expose
private key/token files to Cargo or dependency build scripts.

`import BUILD_SHA256 OCI_SHA256 PROOF_SHA256 PUBLIC_TRUST_FILE HTTPS_GATEWAY TOKEN_FILE` accepts no
signing key. It reads the signed proof and artifact from the gateway, verifies
operator trust and proof binding, and independently checks source/program bytes.
It registers through the existing immutable PG transaction and reads back the
resulting BuildArtifact. `MONDAY_RESEARCH_DATABASE_URL` must refer to the dedicated
Build importer role; installing schema/roles remains an independent operation.

ACR research publication uses `MONDAY_RESEARCH_RELEASE_POLICY`, including its
per-product `oss_by_product` configuration (projected to native `oss`), and the dedicated `MONDAY_RESEARCH_RELEASE_SIGNING_KEY`.
The capability executable exchanges GitHub OIDC directly with RAM STS.
It requests only exact source/Build prefixes from the authenticated native plan.
CI no longer imports PG or requires a release Gateway/Broker endpoint.
ACK uses separate read-only OSS credentials, pinned public trust and an independently signed host admission envelope. Its operator key must differ from every CI release key.
It independently verifies completed GitHub producers and object bytes before immutable PG import.

The cheap `check-presence` receives only booleans. Native signer, policy,
OIDC, exact-scope and authenticated OSS preflight remain mandatory before ACR login.
Publication keys and STS files stay outside Cargo and retained Actions artifacts.
No production credentials, permissions or services are created by this code.

The OSS commands are `oss-check-config`, `oss-publish` and `oss-import`.
Their actual argument forms are documented in the ACK/OSS contract above.
Retained Gateway code still supports runtime AttemptWriter and existing integration tests.
STS has a fifteen-minute minimum and cannot reproduce the old Broker's two-minute revocation lease.
Deployment remains blocked without independent RAM exact-prefix base-role and denial tests, trusted read-only ACK mounts, and a never-versioned existing OSS bucket. Versioning refusal is a constraint of this adapter.
