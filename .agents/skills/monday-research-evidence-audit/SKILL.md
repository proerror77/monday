---
name: monday-research-evidence-audit
description: Audit Monday research evidence from authenticated input through snapshot, Mission admission, evaluator output, immutable publication, and independent readback. Use when checking research E2E readiness or completion, ResearchSnapshot, evaluator/MCTS results, sealed holdout evidence, OSS artifacts, cohort completeness, or claims that a research run succeeded.
---

# Monday Research Evidence Audit

Audit existing evidence only. Do not collect data, run evaluation, publish results, or touch production.

## Workflow

1. Name one research contract, venue/instrument, time window, and expected terminal artifact.
2. Follow immutable identities through every stage:
   - authenticated input manifest and source digest;
   - cohort/partition and `ResearchSnapshot` digest;
   - admitted typed Mission and policy/configuration digest;
   - evaluator or MCTS run identity and terminal result;
   - immutable result bundle and checksum;
   - independent OSS or artifact-store readback of the same bytes and checksum.
3. At each boundary, compare both the referenced identity and the actual content. Record `passed`, `missing`, `mismatch`, `stale`, or `not applicable`.
4. Check that fixtures, synthetic substitutes, unrelated collector health, CI success, and preparation logs are not being used as terminal research evidence.
5. The overall result passes only when every required boundary passes for the same contract and window.

## Safety boundaries

- Never fabricate missing history or completeness. Mark unreconstructable data `missing` and state the excluded window.
- Never use research authority to start collectors, alter deployment, submit orders, change risk limits, or enable Paper/Shadow/Live.
- Do not treat collector deployment, snapshot construction, evaluation, and publication as one rollout unit.
- Do not expose credentials or secret material; record only authenticated status and immutable public identities.

## Stop conditions

Stop following a branch when an identity breaks or cannot be read back. Continue checking independent branches only if that helps locate multiple gaps; the terminal result remains incomplete.

## Output

Return `Stage | Result | Expected identity | Observed identity | Evidence | Gap` for the six stages above.
End with `Overall: passed` or `Overall: incomplete`, followed by the earliest broken boundary and the smallest read-only check needed next.
