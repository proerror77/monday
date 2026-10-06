# Independent Build release publisher

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
HTTPS gateway readbacks. It publishes and reads back `release-proof.json`,
then rechecks mutable GitHub authority before Ed25519 signing. Immutable producer
verification is reused internally; store-boundary readbacks remain independent.
Signed release and BuildArtifact JSON are also published and read back.
Image-specific proofs live under `research/builds/{build}/releases/{oci}/{proof}/`.
Images can share the same compiled programs/Build without overwriting each
other's signed OCI binding. The proof identity also isolates authenticated
publisher attempts, so a new attempt cannot overwrite earlier evidence. Their common executable blobs keep the Build prefix.

`plan SOURCE_ROOT REQUEST POLICY` emits actual Build identities and exact broker
prefixes without signing, uploading, importing or granting a Run. The broker
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

ACR wiring uses `MONDAY_RESEARCH_RELEASE_POLICY` and
`MONDAY_RESEARCH_RELEASE_GATEWAY` and `MONDAY_RESEARCH_RELEASE_BROKER` variables
plus the dedicated `MONDAY_RESEARCH_RELEASE_SIGNING_KEY` secret.
The [per-job capability exchange](RELEASE_CAPABILITY.md) replaces the static
gateway token secret. It obtains Reader scope before registry writes and exact
source/Build Publisher scope after the native plan. Missing configuration blocks
research publication before registry writes. PG projection is explicitly enabled
only by `MONDAY_RESEARCH_RELEASE_IMPORT_ENABLED=true` and requires the dedicated
`MONDAY_RESEARCH_RELEASE_IMPORT_DATABASE_URL` secret. Public plan/artifact
metadata is retained as an Actions artifact. Private key/token files are removed
on every wrapper exit. This change creates none of that production configuration.

The publication job first runs `check-presence` before artifact download and
issuer preparation. GitHub passes only presence booleans for the named variables
and secrets, never their values. It reports all absent settings together using
the actual repository setting names. If PG import is enabled, its importer URL
must also be present before any publication begins. Non-research matrix rows do
not run this check.

Passing this inexpensive check proves only that settings exist. The later native
`check-config` still verifies policy, signer, selected product/repository and TLS
before registry login; presence booleans cannot authorize publication. Cargo
still compiles the issuer before private credentials are injected. A configured
importer must independently verify and read back its immutable PG projection.

An operator must supply the real digest-pinned builder, product repositories and
public trust policy, approve the independent signer and scoped gateway capability,
and, when import is enabled, the dedicated existing PG importer role and URL.
Creating persistent keys, permissions or services is a separate authorized
operation. This preflight creates none, probes no endpoint and grants no Run.
