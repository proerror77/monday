# Automatic research publication

Monday publishes admitted artifacts automatically. Publication does not start
research, resume collection, change risk, or enable trading.

Keep the native `acr-publish.yml` workflow and existing research job names.
Keep all three authenticated checks, current main, original producer identity,
binary smoke, immutable software downloads, signed Build proofs, OCI executable
readback, publication completion, and credential cleanup.
Keep research matrix publication serial because products share a source archive.
Keep Release, both Claude workflows, and the monitor OIDC template unchanged.
Do not install the historical human probe or its global Release hold.

## Identity and admission

Each research product references its own existing environment:

| Product | Environment |
| --- | --- |
| `cex-runner` | `monday-research-cex` |
| `controller` | `monday-research-controller` |
| `prediction-runner` | `monday-research-prediction` |

Each environment permits exactly branch `main`, using a custom branch policy.
The automatic route has no required reviewer, wait timer, or custom protection.
If any such protection exists, the guard rejects this route. It never approves,
removes, or bypasses that protection.

An ordinary repository variable, `MONDAY_RESEARCH_AUTOMATIC_PUBLICATION`, pins
the repository ID, owner ID, actual default subject prefix, and three immutable
environment IDs. The schema is `monday.automatic_research_publication.v1`.
Use the actual readback values. Do not commit account-specific configuration.

The GET-only gate checks those pins, exact run/attempt/source/workflow, current
main, environment rules, and each selected OSS policy subject. It emits an
environment matrix only after every selected product passes. Missing metadata,
API errors, recreated environments, or source drift fail without that matrix.
The publisher repeats these checks before any credential-bearing step.

Ordinary image publication uses a separate matrix without an environment or
OIDC capability. It retains the existing steps and source admission.
Manual recovery retains its existing required checks and producer rules.
An explicit research rebuild also waits for the environment gate.

## Trust boundary

Retain the repository's default OIDC template. This preserves Claude and monitor
authentication. An environment subject distinguishes the product identity.
The existing RAM provider can validate issuer, audience, and subject.
It cannot identify the workflow path from this default environment subject.
Another trusted main workflow could reference the same environment.
Main workflow code, main write permissions, environment administrators, and
configured cloud administrators remain trusted.

Environment GET and job creation are separate operations. If an administrator
deletes an environment after admission, GitHub may recreate its name. The
publisher rejects the changed immutable ID before using cloud credentials.
If an administrator changes settings after the last check, no client check can
provide an atomic lock. This is an administrative trust boundary, not evidence
of a bypass by an unprivileged caller. Keep environment identities and branch
rules stable while their admitted publication runs are active.

## Persistent scope and rollout

The original exact `role_prefixes` configuration binds one source and Build set.
It cannot support future releases without another configuration change.
The OSS contract owner must supply the stable namespace interface separately.
Each runtime session must still use the exact native source/Build plan.

A persistent grant for the existing bucket's `research/builds/*` and
`research/sources/*` is system publication authority. The shared object layout
does not let RAM infer a product from a Build hash. Different roles distinguish
identities; they do not prove server-side isolation between those object hashes.
Never claim the optional session policy restricts a caller who omits it.

Before enabling the identities, approve exact environment and RAM changes once.
Reuse the current provider and STS audience. Do not change repository OIDC,
monitor trust, Claude, signing keys, collectors, or live runtime permissions.
The ordinary OSS map remains owned by its existing configuration writer.
Missing pins or OSS settings leave research publication blocked; they do not
disable Release or ordinary image publication.

Read back every applied identity and policy. Verify native positive and negative
authorization, executable bytes, signed proofs, and registry digests before
claiming publication success. A local fixture is not that native evidence.
Rollback first removes new cloud trust/grants; do not delete an environment
while it remains cloud-trusted. Preserve existing immutable published artifacts.

No cloud writes, new charges, or production transitions are authorized by this
document. Actual storage, requests, and traffic need the approved account budget.
No per-run human approval is added by the implementation.
