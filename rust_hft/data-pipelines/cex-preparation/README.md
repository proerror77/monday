# CEX data preparation

`monday-cex-prepare` owns bounded acquisition and materialization. It does not
freeze Campaigns, sign requests, reserve trials, submit Jobs or train models.
The existing admitted scheduler or operator starts its bounded data task.

## Handoff

1. Run `monday-cex-prepare prepare-fresh-inputs` inside the admitted ACK data Job.
2. Retain its request, selection, frozen inventory, report and output objects.
3. Run `alpha-harness mission verify-prepared-inputs` with the same arguments.
4. Continue the existing Campaign freeze, finalize and dispatch path.

Both commands accept the existing bounded preparation arguments. For example,
the producer and verifier use this exact argument set:

```sh
--raw-root /lake/raw --reference-root /reference/lake/raw \
--market usdm --symbol BTCUSDT \
--start-received-at-ns 1700000000000000000 \
--end-received-at-ns 1700000060000000000 \
--image-ref REGISTRY/research-runner@sha256:IMMUTABLE_DIGEST \
--mission-id fresh-btc --output-root /lake/output --output-prefix campaign/fresh-btc \
--bucket-ms 1000 --label-horizon-buckets 5 --top-depth 5 \
--max-scan-entries 100000 --max-inputs 64 --max-input-bytes 1000000000 \
--inventory-out /campaign-root/frozen.env \
--request-out /lake/output/.fresh-inputs/campaign/fresh-btc/request.json \
--campaign-inputs-out /lake/output/campaign/fresh-btc/receipts/campaign-inputs.json \
--materializer /contract/cex-materialization-entrypoint.sh \
--materializer-work-dir /work/materializer --binary-dir /usr/local/bin \
--materializer-timeout-seconds 7200 --max-materializer-output-bytes 16777216 \
--report-out /campaign-root/preparation.json
```

Replace the example window, roots, budgets and digest with the admitted request.
The source revision comes from each compiled program. Both must use the same
verified source. A caller cannot override that revision through CLI arguments.

`request.json` and `selection.json` remain under
`OUTPUT_ROOT/.fresh-inputs/OUTPUT_PREFIX/`. The materializer publishes its frozen
inventory beside `campaign-inputs.json` and `materialization-receipt.json`.
All output paths and existing create-once semantics remain unchanged.

The verifier reads the actual output bytes and reconstructs the inventory from
the original selection. It checks every request parameter, digest, byte limit,
receipt, and sequence or market feature provenance binding. It requires the
completed report and the published inventory copy. It never creates or repairs
missing evidence. It does not re-read raw source payloads; that remains the
producer's discovery and PIT materialization responsibility. Raw roots and
producer executable paths remain immutable request identity fields. They need
not be mounted in the controller.

The legacy controller retains its `--fresh-*` parameters. `start` and Study
handoff call the readonly verifier with those same expectations. Missing or
invalid evidence writes `needs-preparation.json`, returns a failure, and leaves
child dispatch and handoff consumption untouched. Complete preparation externally
at the reported paths, then repeat the original controller command. No extra
scheduler, retry service or timer is introduced.

For Study handoff, the preparation report remains
`GENERATION_DIR/study/preparation.json`; the request uses the same canonical
`OUTPUT_ROOT/.fresh-inputs/OUTPUT_PREFIX/request.json` rule. The controller does
not prepare the next Study window inside its own Pod.

## Other data commands

- `monday-cex-prepare sources` lists acquisition capabilities.
- `monday-cex-prepare acquire` creates the governed OHLCV trace and manifest.
- `alpha-harness data register-dataset --db DB --manifest FILE` validates the
  actual trace, then registers its immutable metadata.
- `monday-cex-prepare freeze-inventory` creates a frozen archive selection.
- `monday-cex-prepare verify-materialization-manifest` verifies raw/reference
  metadata for the existing materializer entrypoint.

## Packaging and limits

The data workspace builds this program independently. The operator dependency
graph excludes collector, ClickHouse and venue acquisition adapters. The data
program excludes Alpha Engine, dispatch and the control platform. Shared input
validators contain no acquisition or process-launch implementation.

The controller image excludes all materializers and their entrypoint. Existing
paused data Job templates temporarily use `research-runner`, which carries this
program alongside legacy scientific programs. This preserves those consumers;
it does not complete independent data OCI publication or eliminate image rebuild
coupling. Independent OCI, archive/ACK admission and production migration remain
held. This code creates no cloud resources or new permissions.
