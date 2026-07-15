# Polymarket region probe — 2026-07-15

## Outcome

Alibaba Cloud Dubai (`me-east-1`) is the best practical technically non-blocked deployment-region
candidate found for Monday. A temporary Dubai ECS returned `blocked=false`, served public CLOB HTTP
data, and passed Monday's Rust public WebSocket smoke test. The instance and every temporary
dependency were then released and verified absent.

This was a read-only connectivity test. It did not use a wallet, API credentials, account data, or
an order submission.

## Region decision

Polymarket documents its primary servers in AWS `eu-west-2` and its closest non-georestricted
region as AWS `eu-west-1`. Alibaba Cloud's region IDs are unrelated: Alibaba `eu-west-1` is London,
which Polymarket restricts.

| Candidate | Result |
| --- | --- |
| Riyadh `me-central-1` | Fastest public sample at roughly 136–155 ms, but it is an Alibaba partner region and was not the best reproducible account deployment target. |
| Dubai `me-east-1` | Public samples were roughly 167–185 ms and `blocked=false`; selected for the ECS proof. |
| Seoul `ap-northeast-2` | Four public cloud exits returned `blocked=false`, but CLOB latency was roughly 272 ms. |
| Hong Kong `cn-hongkong` | `blocked=false`, but public samples showed materially higher jitter. |
| Tokyo `ap-northeast-1` | The API document calls Japan frontend-only restricted, but three independent live exits returned `blocked=true`; Monday's execution client correctly fails closed on that response. |
| Singapore / Frankfurt / London | Restricted for opening positions; excluded regardless of latency. |

Sources:

- [Polymarket geographic restrictions](https://docs.polymarket.com/api-reference/geoblock)
- [Polymarket trading infrastructure](https://docs.polymarket.com/trading/overview)
- [Polymarket Help Center restrictions](https://help.polymarket.com/en/articles/13364163-geographic-restrictions)
- [Alibaba Cloud ECS regions](https://www.alibabacloud.com/help/en/ecs/user-guide/regions-and-zones)
- [Dubai public latency samples](https://api.globalping.io/v1/measurements/2bvBGmicREmkt8E1m00020lCK)
- [Dubai public geoblock samples](https://api.globalping.io/v1/measurements/2KonmzfD73B3BcMrQ00020lCK)
- [Tokyo public geoblock samples](https://api.globalping.io/v1/measurements/2wJEdPt8d8G3pCwHM00020lCF)
- [Seoul public geoblock samples](https://api.globalping.io/v1/measurements/2HvVxkZOu6VICMCDx00020lCG)

Japan's API-reference classification conflicts with both the Help Center overview and the live
`/api/geoblock` result. The live response remains the fail-closed authority for Monday activation.

## Dubai ECS proof

| Field | Value |
| --- | --- |
| Run ID | `pm-dubai-20260714T162612Z` |
| Instance | `i-eb3g14b63w1mhtnuficj` |
| Region / zone | `me-east-1` / `me-east-1a` |
| Public IP during test | `47.91.37.220` |
| Type | `ecs.mn4.small`, 1 vCPU / 4 GiB — the smallest account-permitted Dubai type whose dry-run passed |
| Image / disk | Ubuntu 24.04 x86_64 / 20 GiB `cloud_efficiency` |
| Billing | `SpotAsPriceGo`, quoted approximately CNY 0.1092/hour plus traffic |
| Network | Temporary VPC, no ingress rules, outbound-only enterprise security group |
| Fail-safe | Automatic release at `2026-07-14T17:56:00Z`; manual release completed first |
| Created | `2026-07-14T16:29Z` |
| Probe | `2026-07-14T16:31:49Z` to `2026-07-14T16:32:15Z` |

The rejected smaller Dubai dry-run returned `InvalidInstanceType.ValueNotSupported`; the selected
type then returned the expected `DryRunOperation` validation success before creation.

Observed results:

- Geoblock: `blocked=false`, country `AE`, region `DU`.
- CLOB `/time`: 5/5 HTTP 200; total latency 181–201 ms, median 188 ms.
- Public order book: HTTP 200 in 186 ms, 4,989-byte response for the active outcome token.
- Cloud Assistant invocation `t-are6qwvu1s19p1c`: `Success`, exit code 0.

## Rust proof

The deployed probe was the Monday Rust crate `hft-data-adapter-polymarket`, built as a statically
linked `x86_64-unknown-linux-musl` ELF from commit
`6b146e9bf6d0c6fea353242670b478e3615da22b`.

The WebSocket snapshot test itself ran through this Rust binary. Cloud lifecycle, artifact transfer,
geoblock, and HTTP latency collection were deliberately orchestrated with Alibaba CLI and shell/curl.

- Test: `tests::live_public_stream_smoke`.
- Result: `1 passed; 0 failed`; a real public CLOB snapshot arrived in 0.46 seconds.
- Compressed artifact SHA-256:
  `74309f00c700c170a439a011510b89258125ba011f3357c15c4e64cf362b7e45`.
- Executable SHA-256:
  `5aea388d8ae8a83364dcb481a2715a3da2467034e4f81d3024fd1162f4d10db0`.

The Polymarket quote and execution hot paths are Rust. The wider Monday repository also contains
supporting Rust, TypeScript, and shell tooling, so it would be inaccurate to call every repository
file Rust.

## Release and cleanup proof

The ECS was gracefully stopped and then deleted. Final readbacks returned `TotalCount=0` for:

- instance `i-eb3g14b63w1mhtnuficj`;
- its system disk and ENI `eni-eb3g14b63w1mhtnsk5kt`;
- security group `sg-eb35zhyj09h90xmoe4f3`;
- vSwitch `vsw-eb369xss0nzzgs3c2oyyo`;
- VPC `vpc-eb3x0aslsk3ku2xrk0ie9`.

The temporary OSS probe object was deleted and a final `stat` confirmed it absent. An earlier empty
Seoul preflight network was also removed before the Dubai deployment; its VPC, vSwitch, and security
group all returned `TotalCount=0`. Cleanup verification finished by `2026-07-14T16:36:43Z`.

## Activation boundary

This proves technical public connectivity from one Dubai IP, not the operator's legal or contractual
eligibility to trade from another physical location. Polymarket prohibits using VPNs or similar
methods to bypass restrictions. Production activation still requires a jurisdiction and operator
setup approved for the intended activity, plus authenticated account/readiness checks. A funded
order should not be used merely as a connectivity probe.
