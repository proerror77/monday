# Campaign checkpoint fixtures

These files are unedited JSON outputs from the existing
`deployment/aliyun/research/test-campaign-cycle-controller.sh` test, using its
fake external ports and real controller process. They are wire-contract
fixtures, not real ACK, OSS, profitability, or unattended-controller evidence.

From the repository root, regenerate all four files with:

```sh
CAMPAIGN_CHECKPOINT_FIXTURE_DIR="$PWD/rust_hft/alpha-harness/domain/tests/fixtures/campaign-checkpoints" \
  bash deployment/aliyun/research/test-campaign-cycle-controller.sh
```

The domain tests deserialize both schemas, preserve every JSON field on a
semantic round trip, verify the completion's hash against the original learning
checkpoint bytes, and reject schema drift and inconsistent transitions. Keep
these original bytes: reformatting a learning checkpoint changes its hash.

This is the first Rust controller contract delivery. It does not implement Job
watch/relist, Lease fencing, ledger/dispatch admission, or independent evidence
readback. Production bash checkpoint semantics remain unchanged.
