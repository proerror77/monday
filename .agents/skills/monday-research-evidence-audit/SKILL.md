---
name: monday-research-evidence-audit
description: Audit one explicitly requested Monday research run from authenticated input through its canonical execution seam, terminal evidence, immutable publication, and independent readback. Use for ResearchSnapshot, evaluator/MCTS, Campaign trial-ledger, baseline Gate, sealed-holdout, cohort-completeness, or research-success claims; do not use for collector health, Gate changes, ordinary code progress, or implementation work. For Code/CI/merge/release/deployment status, use monday-delivery-status instead.
---

# Monday Research Evidence Audit

Audit existing evidence only. Do not collect data, run evaluation, publish results, or touch production.
This Skill reports an existing research result; it is not a workflow prerequisite
for modifying research or collector code.

## Workflow

1. Name one research contract, venue/instrument, time window, canonical execution seam, and expected terminal artifact. The CEX cloud path is `mission campaign-freeze` -> `mission campaign-finalize` -> `mission dispatch submit` -> `mission campaign-execute`; Prediction uses `prediction execute`. Direct `mission execute`, low-level `mission run`, and legacy `loop run` are diagnostics, not alternate completion seams.
2. Follow immutable identities through these seven stages:
   - authenticated input manifest and source digest;
   - venue admission or verifier receipt plus cohort/partition and `ResearchSnapshot` digest;
   - admitted typed Campaign and per-round Mission, policy/configuration digest, and evaluator repository/binary/OCI identity;
   - pre-holdout terminal evidence: evaluator result, or for CEX a passing baseline Gate before subset MCTS; Campaign runs must bind declared total trials to every round and account for consumed trials;
   - explicit final state: no candidate, selected pre-holdout, or finalized; verify round holdouts stayed closed and the selected sealed holdout opened at most once;
   - immutable result bundle and checksum;
   - independent OSS or artifact-store readback of the same bytes and checksum.
3. At each boundary, compare both the referenced identity and the actual content. Record `passed`, `missing`, `mismatch`, `stale`, `unknown`, or `not applicable`. Use `unknown` for authentication, network, permission, or other observability gaps; reserve `missing` for verified absence.
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

Return `Stage | Result | Expected identity | Observed identity | Evidence | Gap` for the seven stages in step 2.
End with `Overall: passed` or `Overall: incomplete`, followed by the earliest broken boundary and the smallest read-only check needed next.
