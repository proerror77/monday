---
allowed-tools: Bash, Read, LS, Task
---

# Run Tests

Run the smallest validation that can disprove the requested change.

## Usage

```text
/testing:run [package, test target, file, or pattern]
```

## Execution

1. Read the nearest manifest and repository instructions. Never invent a
   package or test command.
2. If a target is supplied, run only that target. Otherwise infer the owning
   package from the current diff; do not default to the full repository suite.
3. For Rust, use locked dependencies and the repository Cargo configuration so
   the existing build cache remains active. Never run `cargo clean` first.
4. Use mocks for deterministic unit boundaries. Require real services only when
   the behavior under test actually crosses that boundary.
5. Run an owning-package check only after the focused check passes and only when
   it can still disprove the change. Delegate to the test-runner agent only when
   separate failure analysis will materially help.

## Result

Report the exact command, duration, pass/fail result, first causal failure, and
any boundary not verified. On success, keep the output to one concise summary.

Stop only processes started by this invocation, using captured process IDs or
the test tool's own cleanup. Never use a broad process-name kill.
