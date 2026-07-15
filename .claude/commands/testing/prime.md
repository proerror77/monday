---
allowed-tools: Bash, Read, Write, LS
---

# Prime Monday Testing Environment

Prepare the testing context for this Rust monorepo and its TypeScript operator
frontends. There is no compatibility test runner or second-language fallback.

## Preflight

1. Read the root and nearest product `AGENTS.md` plus `CLAUDE.md`.
2. Confirm the current branch and dirty paths with `git status --short`.
3. Locate the relevant manifest before inventing a command:

   ```bash
   find . -maxdepth 4 \( -name Cargo.toml -o -name package.json \) -print
   ```

4. Identify the smallest changed Rust package or TypeScript frontend.
5. Check external prerequisites only when the target actually needs them. A
   missing database, credential, cloud role, venue, or system tool is an
   environment boundary, not an empty-data result.

## Rust lanes

Use explicit manifests and locked dependencies:

```bash
cargo test --manifest-path rust_hft/Cargo.toml -p <package> --locked
cargo clippy --manifest-path rust_hft/Cargo.toml -p <package> --all-targets --locked -- -D warnings
cargo fmt --manifest-path rust_hft/Cargo.toml --package <package> -- --check

cargo test --manifest-path products/ploy/Cargo.toml -p <package> --locked
cargo clippy --manifest-path products/ploy/Cargo.toml -p <package> --all-targets --locked -- -D warnings
cargo fmt --manifest-path products/ploy/Cargo.toml --package <package> -- --check
```

During diagnosis, prefer one test target or name filter. Expand to the package,
feature matrix, or workspace only when the affected boundary warrants it. PLOY
ordinary validation must not require a local PostgreSQL instance.

## TypeScript frontend lanes

Read the relevant `package.json` scripts, then use the declared command. The PLOY
operator frontend currently uses:

```bash
npm --prefix products/ploy/ploy-frontend ci
npm --prefix products/ploy/ploy-frontend run contracts:check
npm --prefix products/ploy/ploy-frontend run lint
npm --prefix products/ploy/ploy-frontend run build
```

## Execution rules

- Preserve unrelated user changes and do not delete caches or fixtures to make a
  test pass.
- Do not silently enable a feature, synthetic model, mock service, or live path.
- Record the exact command, pass/fail count, duration, warnings, and first causal
  failure.
- Separate formatting, compilation, unit/integration behavior, database-backed
  proof, remote deploy state, and live-runtime truth.
- A local pass cannot prove a collector is deployed or a trading venue is safe.

## Output

```text
Test execution summary
- Command: <exact command>
- Result: <pass/fail/skipped, count, duration>

Failures
- <target>: <first causal error and source location>

Warnings and boundaries
- <warning, missing external proof, or none>

Next verification
- <smallest justified follow-up>
```

$ARGUMENTS
