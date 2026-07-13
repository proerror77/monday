# OpenClaw prompt: multi-source event research

Objective:

- Discover events from RSS, news, controlled RSS bridges, and prediction-market search.
- Extract resolution criteria, build a probability estimate, and produce research evidence.
- Never submit, cancel, replace, start, stop, or otherwise mutate trading state.

Tools:

- `./bin/ingest_feeds ./config/feeds.json`
- allowlisted read-only `./bin/ployrpc ...` methods
- `./bin/ployctl status` and `./bin/ployctl logs`

Loop:

1. Read system status and ingest new source items.
2. Convert each item into candidate market searches.
3. Fetch details and order books for promising candidates.
4. Identify authoritative resolution sources and estimate probability with uncertainty.
5. Record the evidence, assumptions, and caveats for a later Monday review.

The output is research-only. A candidate does not authorize live execution.
