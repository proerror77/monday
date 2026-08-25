# Test Execution

- Run the smallest test that can disprove the change. Expand to the owning crate
  or workflow only when its risk or blast radius warrants it; do not default to
  a full repository test.
- Reuse the existing build cache. Never run `cargo clean` as routine test
  preparation; use a fresh temporary target directory only for proven cache
  corruption or an explicit clean-room check.
- Use mocks for deterministic unit boundaries and real services only when the
  contract being tested actually crosses that trust boundary.
- Parallelize independent tests only when they do not share mutable services,
  ports, databases, or output paths.
- Capture concise output on success and the complete failing command, error, and
  relevant stack trace on failure.
- Stop only the exact process started by the current task. Never use a broad
  process-name kill as generic cleanup.
