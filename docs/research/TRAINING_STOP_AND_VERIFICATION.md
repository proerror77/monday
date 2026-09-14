# Training stop and verification contract

New `MlpOptimizationControlsV1::default()` requests enable `stop_on_convergence`.
The setting is frozen and hashed with the training request. Historical controls
without that field retain their fixed-budget meaning and serialization.

Training checks only post-update training loss, at the declared window boundaries
after the minimum update count. All declared adjacent window changes and the
full tail range must satisfy the convergence policy. It stops at the first
eligible stable checkpoint. Nonfinite loss/parameters/gradients, excessive raw
gradient norm or loss growth still reject before another unsafe update. Global
gradient clipping retains the existing numerical bound.

Evidence records the maximum requested budget, actual completed updates, complete
loss/gradient history and one of `training_converged`,
`update_budget_exhausted_not_converged`, or the historical
`fixed_update_budget_completed`. Load-time validation recomputes the stop decision
and rejects early exits, rewritten reasons or training beyond an eligible stop.
Save/load and portable prediction retain original return units.

A stable training plateau does not prove predictive or economic value. Models
that declare convergence stopping but exhaust their budget without convergence
retain their statistical evaluation as negative evidence and cannot be selected,
frozen for final evaluation or accepted as a selected model during result
readback. Existing cost, predictive and trading evaluation gates still apply to
models that converge. Validation/selection/holdout labels do not select the
training stopping point.

`prepare_cex_baselines` performs primary training plus one independent refit and
returns an immutable `VerifiedCexBaselineRun` tied to its borrowed input context.
Supervised evaluation consumes those admitted models without a third fit. The
type cannot be deserialized or mutated, and the public untrusted-artifact
evaluation API still performs its full verification. This is in-process reuse;
it is not optimizer checkpoint recovery after a worker crash.

These changes implement the training lifecycle, not a new research experiment.
The next research direction remains H1/Ridge with its separate pre-fit label
screen, unchanged factors/cost gate and unseen-calendar check. No model family,
same-window trial, real-data download or ACK run is authorized by this document.
