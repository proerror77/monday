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

## Native broker

The native `research-release-capability-broker` implements the exchange.
It is code only. This change does not install, configure or deploy it.
Build it from this owning manifest with the `publisher` feature.

The broker accepts `--config CONFIG.json`. Configuration contains:

```json
{
  "bind": "127.0.0.1:8092",
  "endpoint": "https://gateway.example/broker/release-capability",
  "policy_file": "/var/lib/monday-broker/publisher-policy.json",
  "repository_id": 123,
  "owner_id": 456,
  "capabilities_file": "/var/lib/monday-identity/capabilities.json",
  "replay_file": "/var/lib/monday-broker/issuance.json",
  "scratch_root": "/var/lib/monday-broker/scratch",
  "publisher_binary": "/usr/local/bin/research-release-publisher",
  "verifier_sandbox": "/usr/bin/bwrap",
  "tools_path": "/usr/local/bin:/usr/bin:/bin"
}
```

The IDs above are placeholders. Resolve actual immutable IDs through GitHub.
The endpoint must match the client audience exactly.
Use its canonical HTTPS form, without an explicit default port.
An existing TLS ingress must route that path to the loopback listener.
Keep gateway and broker on the same configured TLS origin.
Ingress must allow the bounded 150-second verification request.

The operator owns configuration, projection, journal and their private parents.
Directories require mode 0700; state files require mode 0600.
Initialize a new projection to `[]` only when no gateway identities exist.
Initialize the journal to `{"schema":1,"replays":{},"issued":{}}`.
The broker shares the gateway issuer's `identity.lock` sidecar.
Only one broker process may own a journal.
It acquires a journal lock and binds the listener before startup revocation.
It preserves unrelated AttemptWriter capabilities during updates and recovery.
Never copy a runner token into either state file.

GitHub RS256 verification uses the fixed issuer and fixed GitHub JWKS URL.
The broker rejects other algorithms, ambiguous keys and caller-selected key URLs.
It binds immutable repository/owner IDs, main, workflow, source, run and attempt.
The signed `check_run_id` must match the independently fetched publisher job.
The job name must match the policy's selected image repository.
It authenticates current main, three required GitHub Actions checks and software
producer inputs before issuance. It repeats mutable checks after native planning.

Publisher and import scopes require an independently computed native `plan`.
The broker downloads the authenticated compiler artifact from GitHub.
It checks out the exact source and reuses the existing native publisher verifier.
Caller prefixes and hashes select requests; they never define allowed authority.
The short-lived job read token authenticates GitHub API and Git reads.
Git receives it through process environment, never command arguments or git files.

Planning requires Linux user, PID and mount namespaces plus installed `bubblewrap`.
Install trusted regular executables under the configured tool roots.
The required tools include git, bash, gh, jq, unzip, Python and the native publisher.
The sandbox exposes read-only tool/CA roots and a disposable work directory.
It leaves `/proc` empty and clears inherited environment.
Projection, journal and TLS private keys must stay outside mounted tool roots.
The child receives no signing key, ACR password or broker state.
Missing tools or failed namespace isolation deny issuance; there is no fallback.
Two native plans may run concurrently; requests have a 150-second bound.

Each response grants an initial lease of at most two minutes.
One monitor serves all capabilities issued to the same signed publisher job.
Every minute it reads that job independently.
Main/run renewal snapshots are shared for at most 55 seconds.
Issuance and its final authorization read always bypass that cache.
It renews active leases up to the requested one-hour deadline.
Completion, cancellation, source drift or authority-read failure stops renewal
and removes that capability from the gateway projection.
Loss of the broker leaves a maximum two-minute lease.
Restart removes journal-owned capabilities before accepting requests.
Revoked or expired capabilities cannot reappear through renewal.
OIDC replay identifiers and bearer tokens enter state only as SHA-256 hashes.

Production acceptance still requires the chosen Linux host, real TLS ingress,
operator policy/signing trust, original compiler artifact and gateway readback.
Unit tests and a namespace probe do not prove a deployed release.
RSA tests require OpenSSL and generate fresh test keys through in-memory pipes.
They never write or track a private key file.
These test keys do not configure production trust.

## Gateway integration

The broker must share the actual host-owned projection read by the gateway.
A successful HTTP response alone does not establish that integration.

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
