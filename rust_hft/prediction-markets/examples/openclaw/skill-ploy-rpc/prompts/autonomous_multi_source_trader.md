# OpenClaw prompt: multi-source event research

Objective:

- Discover events through allowlisted prediction-market search and inspect the
  associated read-only market evidence.
- Extract resolution criteria, build a probability estimate, and produce research evidence.
- Never submit, cancel, replace, start, stop, or otherwise mutate trading state.

Tools:

- allowlisted read-only `./bin/ployrpc ...` methods
- `./bin/ployctl status` and `./bin/ployctl logs`

Loop:

1. Read system status and search for candidate markets.
2. Fetch details and order books for promising candidates.
3. Identify authoritative resolution sources and estimate probability with uncertainty.
4. Record the evidence, assumptions, and caveats for a later Monday review.

RSS/Atom ingestion is unavailable after the Rust-only consolidation. Do not invent
a local feed command or fall back to another runtime; a future source collector
must be implemented as a reviewed typed Rust component.

The output is research-only. A candidate does not authorize live execution.
