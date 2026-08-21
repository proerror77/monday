# Tokyo bare-ECS trading host contract

This contract prepares the existing `monday/hft-trading` ACR image for a future
dedicated Tokyo ECS. It does not create an ECS, start a stopped ECS, add a public
IP, start ClickHouse, use ACK, enable `LiveSmall`, inject a real credential, or
send an order.

## Supported boundary

- Host: dedicated Alibaba Cloud ECS in `ap-northeast-1`, Ubuntu 26.04, amd64.
- Orchestrator: none. The trading runtime uses Docker and a static systemd unit
  directly on the host; it is not an ACK workload.
- Host authority: the instance must expose the exact RAM role
  `MondayTradingEcsRole` through ECS metadata v2. Attach only the permissions
  needed to retrieve the reviewed secret objects and operational evidence.
- Image: Tokyo Personal Edition ACR repository `wildcard0923/hft-trading`, pulled
  through its VPC endpoint and pinned as `repository@sha256:<64-hex>`.
- Activation: signed, content-addressed Paper or Shadow deployment artifacts.
  Both execute through the runtime's simulated Paper path. `StartLiveSmall` is
  rejected by both this host policy and `hft-live`.
- Boot state: stopped. `hft-trading-ecs.service` deliberately has no `[Install]`
  section and `Restart=no`; staging cannot start it and an ECS reboot cannot
  auto-start it.

Personal ACR is sufficient for the current three-image scale. The host contract
does not trust a tag at runtime: every publication also creates a unique
`run-<github-run-id>-<attempt>` retention tag so a later rebuild of the same
source SHA cannot make the reviewed manifest unreferenced. The workflow records
the pushed OCI digest and packages a deterministic control archive into
`hft-trading-ecs-linux-amd64-<source-sha>`. The release manifest binds the exact
source revision, public publish repository, VPC pull repository, image digest,
control manifest, control archive, architecture, region, and host version.
Checkout, Buildx setup, ACR login, image build/push, and artifact upload all use
reviewed full commit SHAs instead of mutable action tags. The publish job logs
out of ACR and removes its Docker credential file on every completion path.

## Credentials and RAM role

Repository files never contain ACR or venue credentials. An operator-controlled
Cloud Assistant command may use the instance RAM role to retrieve reviewed
secret material, but it must materialize only these ephemeral files:

```text
/run/monday/trading-secrets/runtime.env
/run/monday/trading-secrets/feedback-signing-key.hex
```

`/run` must resolve to `tmpfs`. Staging creates the dedicated non-login system
account and group `mondayhft`; it rejects UID or GID `1000`, so the ordinary
Ubuntu login account can never inherit trading-runtime access. It also rejects
supplementary group members and any other passwd entry reusing the runtime UID
or primary GID. The secret root
is root:`mondayhft` mode `0750`, and both `runtime.env` and the feedback key are
root:`mondayhft` mode `0440`. The feedback key contains one 32-byte Ed25519 key
encoded as 64 lowercase hex characters. The runtime environment must contain a
gRPC token of at least 32 characters with no edge whitespace plus either paired
venue API-key/secret entries or one `HFT_SECRET_BINANCE_ACCOUNT_JSON` object
containing exactly `runtime_account_id`, `api_key`, and `secret`. The wrapper
derives the legacy Binance process variables inside the container and never
stores a second Binance credential copy. It rejects
missing, empty, persistent, path-traversing, overly broad, or malformed secret
inputs before Docker starts. The filesystem is checked separately for the root
and each file, so a disk-backed file bind-mounted below a tmpfs directory is not
accepted. It bind-mounts `runtime.env` read-only and imports
each validated assignment inside the unprivileged container without evaluation;
it never uses Docker `--env-file`, so secret values are not copied into Docker
inspect or daemon metadata. The deployment-envelope signing key is never a host
input. Per-activation state is owned by `mondayhft` mode `0700`. Container and
systemd core dumps are disabled so process-environment credentials cannot be
persisted through a crash dump.

The ACR login password is a separate, root-owned mode-`0400` one-line direct
child of the canonical mode-`0700` tmpfs directory `/run/monday/acr-auth/`.
Symlinks, `..`, alternate spellings, nested paths, and disk-backed files are
rejected. It is used only as
`docker login --password-stdin` with a one-run `DOCKER_CONFIG` on that same
tmpfs. The host then logs out and deletes the temporary Docker configuration; an
exit trap retries both actions on every failure path. Rotate the currently
exposed registry password before this path is used on a real host and update the
GitHub `ACR_PASSWORD` secret; never pass it in a command argument.

## Activation bundle

The operator supplies an absolute, canonical, root-owned activation directory
below the canonical activation root. Runtime validation walks every component
from `/` through the activation root to the final directory; every component
must be a real root-owned directory, never a symlink, with no group/world write
bit. `activation.sha256` must list every other regular file exactly once in
sorted order and at least contain:

```text
config/system.yaml
deployment/bundle.json
deployment/envelope.json
deployment/policy.json
deployment/trusted-keys.json
```

Formula assets referenced by `bundle.json` live under the same directory
and must also be in `activation.sha256`. Only the root manifest excludes itself;
a nested file also named `activation.sha256` is ordinary bundle content and must
be listed and hashed. The signed envelope must contain exactly one JSON value,
exactly one `LoadFactor`, and exactly one of `StartPaper` or
`StartShadow`. The runtime policy must authorize that same artifact and the same
start intent, carry the matching approval class, and be non-paused.
`LoadAllocatorPolicy`, unknown intents, live-small intent, and live-small
approval all fail closed. `hft-live` still performs the authoritative signature,
policy, hash, scope, nonce, risk, and bundle checks.

## Staging and cutover

Download the release artifact for the reviewed source SHA, verify its GitHub
artifact provenance, then copy it into a root-owned, non-group/world-writable
child directory under `/opt/monday/incoming/hft-trading/`. The GitHub artifact is
retained for 90 days; the ACR digest itself remains the durable runtime
publication. On the stopped future ECS:

```bash
sudo deployment/aliyun/trading-ecs-hostctl.sh stage \
  /opt/monday/incoming/hft-trading/<source-sha> \
  '<acr-user>' \
  /run/monday/acr-auth/password
sudo rm -f /run/monday/acr-auth/password
```

Before it pulls an image, creates an account, or installs a file, `stage` rejects
any existing active runtime or unit state other than explicit `static`,
`disabled`, or a proven absent unit; `enabled`, `linked`, `alias`, `indirect`,
`generated`, and ambiguous states fail closed. It then verifies ECS metadata v2,
Tokyo, Ubuntu 26.04, amd64, absence of kubelet,
RAM role, release schema/checksums, the deterministic control archive, and the
pulled `RepoDigests`. It creates or validates the dedicated `mondayhft` account,
installs the unit, and requires `systemctl is-enabled` to report the exact
`static` state (systemd returns success for this state). It records `stage.json`
plus an adjacent `STAGED.sha256`; it never writes the current activation pointer
and never starts or enables the service. The installed runtime, hostctl, policy,
and systemd unit must each hash to the selected release's four-entry control
manifest. `ExecStartPre` repeats that binding and revalidates Ubuntu 26.04,
amd64, Tokyo metadata-v2 identity, the exact RAM role, and the absence of kubelet
on every manual start; a host or control-plane drift therefore cannot start the
container. Staging and `ExecStartPre` also reject every
`monday-hft-trading.service.d` directory and require systemd's effective
`FragmentPath`, empty `DropInPaths`, static state, `Restart=no`, empty
`ExecStartPost`, and exact preflight/run/stop command vectors to match the
selected unit. A stale or newly injected drop-in therefore cannot override the
hashed unit silently.

After independent review of the signed activation and ephemeral secret injection:

```bash
sudo /usr/local/sbin/monday-hft-trading-hostctl cutover \
  'crpi-INSTANCE-vpc.ap-northeast-1.personal.cr.aliyuncs.com/wildcard0923/hft-trading@sha256:<digest>' \
  '<hft-trading-ecs-release.json sha256>' \
  /opt/monday/activations/<activation-id>
```

Stage, cutover, and rollback share a non-blocking host lock, and every evidence
run uses an exclusively created directory. Cutover is accepted only while the
unit and exact-name Docker container are absent. It reruns the complete
preflight, starts the exact locally staged digest with `--pull never`, freezes
the systemd `InvocationID`, main PID, restart count, Docker container ID, running
state, image digest, and candidate-owned loopback ports. Docker publishes the
candidate's metrics port `9090` and authenticated gRPC port `9092` onto separate
random `127.0.0.1` host ports; neither service is publicly exposed, and health
never trusts an unrelated process already listening on a conventional host
port. Container identity acquisition is bounded to tolerate normal Docker port
publication latency. The gate rechecks that complete identity around two clean
`/health` and `/readiness` samples. Docker stop uses `SIGINT`, matching
`hft-live`'s graceful shutdown path so cancellation and reconciliation run
before the container is removed. It
rechecks once more immediately before atomically
committing the single-file `PASSED.sha256` marker. No marker means no successful
cutover; an uncommitted JSON file is explicitly marked `cutover.unconfirmed.json`.

Use the exact successful cutover directory for a one-shot governed readback:

```bash
sudo /usr/local/sbin/monday-hft-trading-hostctl readback \
  /var/lib/monday/evidence/hft-trading/cutover/<run-id>
```

`readback` never starts, stops, restarts, or enables the service and contains no
retry loop. It accepts only an unrevoked `PASSED.sha256`, the same current
pointer and systemd/container identity recorded by cutover, and a fresh pass of
the installed production preflight. It then binds the merged governance
identities (`deployment_id`, `asset_revision_id`, `promotion_id`, `bundle_id`,
bundle hash, risk-policy hash, and nonce hash) to the consumed nonce, the exact
verified/prepared/activated audit sequence, and the signed activation-feedback
wrapper in the activation's private state directory. Any missing, stale,
tampered, restarted, rolled-back, or cross-activation input exits once with a
failure instead of polling. The shell reports the feedback content hash and
signature presence; Ed25519 verification remains the governance ingestion
authority and is not reimplemented in host shell code.

On any failure or shell `EXIT`, `TERM`, or `INT` after cutover becomes armed, the
candidate is stopped, the prior non-secret pointer is restored, and an atomic
`FAILED.sha256` evidence marker is written. Cleanup ignores a second HUP, INT,
or TERM after it begins. `FAILED.sha256` is emitted only after both stop and
pointer restoration are proven; otherwise the non-canonical
`EMERGENCY_FAILED_OPEN.sha256` marker blocks trading and demands immediate manual
recovery. (`SIGKILL` and host power loss cannot run userspace cleanup; absence of
`PASSED.sha256` still fails closed.) The prior
runtime is intentionally not auto-restarted: its nonce is consumed, so a
rollback restart requires a newly signed envelope and nonce. Explicit rollback
has the same stopped-state rule:

```bash
sudo /usr/local/sbin/monday-hft-trading-hostctl rollback \
  /var/lib/monday/evidence/hft-trading/cutover/<run-id>
```

Successful cutover evidence includes root-only mode-`0400`, content-addressed
candidate and previous pointer snapshots and their hashes. Rollback first proves
that `PASSED.sha256` commits
that evidence, the current pointer still equals the candidate snapshot, and the
active systemd invocation, PID, restart count, container ID, digest, and bound
ports still equal the cutover identity. It repeats pointer and runtime identity
checks immediately before stop. Only then does it atomically append a
`PASSED.rollback-pending.sha256` revocation marker, stop the runtime, restore the
prior pointer if one existed, and append `PASSED.rolled-back.sha256` plus
content-addressed rollback evidence. The original `PASSED.sha256` remains
immutable historical evidence; either rollback marker revokes it as current
authorization. Legacy, tampered, or stale evidence cannot stop a newer runtime.
An interruption leaves the rollback-pending revocation visible. Rollback cannot
enable the service or resume trading.

## Offline verification

```bash
deployment/aliyun/test-trading-ecs-host-contract.sh
cargo test --manifest-path rust_hft/Cargo.toml \
  -p hft-live --no-default-features --test deployment_envelope --locked
cargo test --manifest-path rust_hft/Cargo.toml \
  -p hft-live --no-default-features --test deployment_artifacts --locked
```

The shell contract tests prove runtime rejection of tags,
public/non-Tokyo/wrong-repo references, multiple envelope JSON values, nested
unhashed files, mismatched Paper/Shadow intents, allocator/live-small authority,
drifted control assets, stale rollback lineage, and absent credentials. They
also pin the static unit, no-restart behavior, immutable ACR workflow output,
and rollback-marker semantics. These are offline proofs only; they do not
provision a host or prove venue acceptance.
