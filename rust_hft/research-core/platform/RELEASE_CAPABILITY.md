# Per-job release capabilities

The CI publisher exchanges GitHub job identity through a trusted operator broker.
It never creates a token or expands gateway authority itself.
No broker is provisioned by this change. An absent broker blocks publication.

`MONDAY_RESEARCH_RELEASE_BROKER` is a public HTTPS endpoint variable.
It must share the configured gateway's TLS origin. The existing policy supplies
server trust, including an optional private CA or TLS identity. Hostname checks,
redirect rejection and proxy isolation remain enabled.

The native `research-release-capability` executable has three operations:

```text
source POLICY CONTEXT HTTPS_BROKER HTTPS_GATEWAY TOKEN_FILE
publish POLICY CONTEXT PLAN HTTPS_BROKER HTTPS_GATEWAY TOKEN_FILE
read POLICY CONTEXT PLAN HTTPS_BROKER HTTPS_GATEWAY TOKEN_FILE
```

The wrapper prepares public context from the selected source/product and the
authenticated publisher job ID. The context contains repository, source SHA,
product, full image repository, software run ID, publisher run/attempt/job IDs.
All IDs must be positive; the repository/product must match operator policy.

`source` requests Reader access for the exact source prefix before registry writes.
Native signer/policy/TLS preflight still runs before ACR login.
After registry readback, `plan` computes actual Build identities and the real OCI
binding. `publish` derives exact source/Build prefixes from those BuildSpec inputs.
It rejects extra prefixes, duplicate Builds and foreign source/compiler inputs.
`read` renews Reader access before optional PG import. Import stays opt-in.

The client obtains a job OIDC token from GitHub's HTTPS Actions request endpoint.
The OIDC audience is the complete broker URL. Only the publication job receives
`id-token:write`; the existing GitHub token remains read-only. The client sends
OIDC identity as the broker request's bearer token. It sends no Build signing key,
ACR password, permanent gateway token or scientific grant.

The `X-Monday-GitHub-Read-Token` header delegates that job's short-lived,
read-only GitHub token to the same operator TLS origin. This lets the broker
authenticate and download the original Actions inputs without another permanent
GitHub credential. The broker must verify OIDC before using this header, restrict
its use to GitHub API/artifact reads, and never log or persist either credential.
This delegation is part of the operator's broker trust configuration.

The POST body is JSON with this shape:

```text
schema: 1
context: { repository, source_sha, product, image_repository,
           software_run_id, publisher_run_id, publisher_run_attempt,
           publisher_job_id }
phase: source | publish | read
publisher_prefixes: sorted exact source/Build prefixes
image: null for source; actual repo@sha256:digest otherwise
plan_sha256: null for source; identity(native plan) otherwise
expires_ms: client UTC time plus one hour
```

The broker response is:

```text
schema: 1
request_sha256: SHA-256 of the exact JSON request bytes
expires_ms: strictly future, no later than the requested deadline
role: reader for source/read; publisher for publish
prefixes: exactly the requested sorted prefixes
token: opaque bearer capability
```

The client validates the response binding, role, exact scope and one-hour limit.
It bounds responses to 64 KiB and bearer tokens to 32–4096 ASCII graphic bytes.
Errors never include response/token contents. It writes only a new mode-0600 file
inside a private canonical directory. It refuses existing files and symlinks.
The wrapper removes the file on every exit. No token enters retained artifacts.

## Required broker implementation

The response is a transport contract, not proof that server authorization exists.
The operator broker must enforce the matching gateway projection itself.
Before issuance it must verify GitHub OIDC signature/JWKS, issuer, audience,
expiry, immutable repository/owner IDs, main and permitted workflow identity.
It must independently authenticate run/attempt/job, current source and required
checks. It must derive allowed product/Build scope from immutable producer inputs;
caller-supplied prefixes or a plan hash are insufficient authorization.
It must reject replay outside that job lifetime and revoke issued capabilities.

The gateway remains the object authority. Its existing projection accepts only
an exact source/Build publisher prefix and at most 24 hours of remaining lifetime.
The client requests a stricter one-hour lifetime. Store only token hashes in the
host-owned mode-0600 projection and replace it atomically. Reader requests must
never become Publisher or AttemptWriter grants.

The broker contract does not require a PG database. The current **gateway binary**
does require PG at startup. That dependency belongs to its existing AttemptWriter
implementation; this client neither adds PG nor enables platform authority.
An operator may integrate an existing compliant broker/gateway implementation.
Choosing hosting, adding a publisher-only storage adapter or changing raw OCI
staging policy are separate decisions. No new instance is required by this PR.
