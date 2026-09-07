---
name: monday-research-evidence-audit
description: Audit an existing Monday research run's input, terminal evidence, publication, and logs. Excludes implementation progress, collector health, and source-delivery status.
---

# Monday Research Evidence Audit

Audit existing evidence only. Do not collect data, run evaluation, publish results, or touch production.
This Skill reports an existing research result; it is not a workflow prerequisite
for modifying research or collector code.

## Workflow

1. Name one research contract, venue/instrument, time window, canonical execution seam, and expected terminal artifact. The CEX cloud path is `mission campaign-freeze` -> `mission campaign-finalize` -> `mission dispatch submit` -> `mission campaign-execute`; Prediction uses `prediction execute`. Direct `mission execute`, low-level `mission run`, and legacy `loop run` are diagnostics, not alternate completion seams.
2. Derive required stages from that run's contract and evidenced execution path. Use the following identity boundaries where applicable:
   - authenticated input manifest and source digest;
   - venue admission or verifier receipt plus cohort/partition and `ResearchSnapshot` digest;
   - admitted typed Campaign and per-round Mission, policy/configuration digest, and evaluator repository/binary/OCI identity;
   - pre-holdout terminal evidence: evaluator result, or for CEX a passing baseline Gate before subset MCTS; Campaign runs must bind declared total trials to every round and account for consumed trials;
   - explicit final state: no candidate, selected pre-holdout, or finalized; verify round holdouts stayed closed and the selected sealed holdout opened at most once;
   - immutable result bundle and checksum;
   - independent OSS or artifact-store readback of the same bytes and checksum.
   Mark a stage `not applicable` only when the contract and terminal evidence
   justify its omission, such as replay after an evidenced no-candidate result.
   Absent evidence alone never makes a stage optional. Preserve required terminal
   publication and readback even when research ends without a candidate.
3. Audit the process log independently from the immutable evidence chain. For
   applicable stages, require `monday.research_event.v1` events, or the
   shell-compatible equivalent, for:
   - run identity and research scope: Campaign/Mission/round, venue, instrument,
     horizon, hypothesis, input and policy identities;
   - stage start, bounded progress where needed, and the actual completion or
     failure outcome; stages may include validation, slicing, PIT materialization,
     factor search, Factor Bank, the selected model family, OOS position
     evaluation, L2 replay, result publication, and readback;
   - factor formula plus screening verdict, model aggregate metrics plus equity
     ledger identity, replay metrics plus Gate failures, Campaign termination,
     and bounded LLM follow-up identity;
   - no credentials, signed URLs, labels, holdout contents, or raw market rows.
   Logs explain live progress but never replace a receipt, content digest,
   terminal result, or independent readback. Mark a completed stage with no
   corresponding process events `log_missing` even when its artifacts pass.
4. At each boundary, compare both the referenced identity and the actual content. Record `passed`, `missing`, `mismatch`, `stale`, `unknown`, `log_missing`, or `not applicable`. Use `unknown` for authentication, network, permission, or other observability gaps; reserve `missing` for verified absence.
5. Check that fixtures, synthetic substitutes, unrelated collector health, CI success, and preparation logs are not being used as terminal research evidence.
6. The evidence audit passes only when every required identity boundary passes for the same contract and window. An evidenced no-candidate or negative-return result may pass this audit; it does not prove profitability or deployment readiness. Report log completeness separately so an observability failure cannot be mistaken for an artifact-integrity failure.

## Safety boundaries

- Never fabricate missing history or completeness. Mark unreconstructable data `missing` and state the excluded window.
- Never use research authority to start collectors, alter deployment, submit orders, change risk limits, or enable Paper/Shadow/Live.
- Do not treat collector deployment, snapshot construction, evaluation, and publication as one rollout unit.
- Do not expose credentials or secret material; record only authenticated status and immutable public identities.

## Stop conditions

Stop following a branch when an identity breaks or cannot be read back. Continue checking independent branches only if that helps locate multiple gaps; the terminal result remains incomplete.

## Output

Return `Stage | Result | Expected identity | Observed identity | Evidence | Gap` for the run's identity boundaries, explaining any `not applicable` stages. Report applicable log stages separately.
End with `Evidence audit: passed` or `Evidence audit: incomplete` and the research outcome separately. If incomplete, name the earliest broken boundary and the next read-only check; otherwise state that no evidence gap was found.
