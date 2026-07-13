# PLOY data and collector boundary inside Monday

Monday's governed production data acquisition path is `rust_hft/tools/collector`.
PLOY collectors, loaders, scripts, and research workflows remain product/research
assets inside `products/ploy`; they are not current remote-host deployment authority.

## Current use

- use `crates/ploy-market-data` for PLOY market-data contracts and focused local
  development;
- use `crates/ploy-feed-loaders` for historical research/backtest loading;
- use retained scripts for offline analysis or compatibility only when their inputs
  and outputs are explicit;
- route durable production collection changes through Monday's collector ownership
  and deployment review.

Focused validation:

```bash
cargo +1.91 check --locked -p ploy-market-data --no-default-features --lib
cargo +1.91 check --locked -p ploy-feed-loaders --lib
cargo +1.91 test --locked -p ploy-market-data
```

Database-backed, hosted-artifact, and heavy research lanes run through the root
`.github/workflows/ploy-ci.yml`. Do not dispatch nested PLOY workflows or mutate the
former Tango/trade hosts from this repository.

The former standalone collector/deploy runbook remains recoverable from source SHA
`8ce4e0f150173a44030294101f4b1371cbdf80bc` and the verified pre-archive Git bundle.
