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
   rg --files -g Cargo.toml -g package.json rust_hft
   ```

4. Identify the smallest changed Rust package or TypeScript frontend.
5. Check external prerequisites only when the target actually needs them. A
   missing database, credential, cloud role, venue, or system tool is an
   environment boundary, not an empty-data result.

## Rust lanes

Use `rust_hft/workspaces.json` and the package's declared owner to choose the
manifest. The old root manifest is not a generic entry for every package.
Use the existing `rust_hft/scripts/cargo-scoped.sh` for explicit package sets;
its dry-run still invokes Cargo metadata. Static command selection reads the
existing registry and manifests without compiling.

From the repository root, use locked dependencies and the owning manifest:

```bash
cargo test --manifest-path rust_hft/<owning-manifest> -p <package> --locked <filter>
cargo clippy --manifest-path rust_hft/<owning-manifest> -p <package> --all-targets --locked -- -D warnings
cargo fmt --manifest-path rust_hft/<owning-manifest> --package <package> -- --check
```

The Prediction workspace remains `rust_hft/prediction-markets/Cargo.toml`.

During diagnosis, prefer one test target or name filter. Expand to the package,
feature matrix, or workspace only when the affected boundary warrants it. PLOY
ordinary validation must not require a local PostgreSQL instance.

## TypeScript frontend lanes

Read the relevant `package.json` scripts, then use the declared command. The PLOY
operator frontend currently uses:

```bash
npm --prefix rust_hft/prediction-markets/ploy-frontend ci
npm --prefix rust_hft/prediction-markets/ploy-frontend run contracts:check
npm --prefix rust_hft/prediction-markets/ploy-frontend run lint
npm --prefix rust_hft/prediction-markets/ploy-frontend run build
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
