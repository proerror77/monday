#!/usr/bin/env bash
# Cargo discovers docs, including indented blocks and include_str! attributes.
# Skip exact unit-test names when running Cargo so only doctests execute here.
loop_nextest_doc_counts() {
  awk '
    /^[[:space:]]*Doc-tests / {doc=1; next}
    /^[[:space:]]*Running / {doc=0}
    doc && /: test$/ {listed++}
    doc && /^test result: ok\./ {passed += $4}
    END {printf "%d %d\n", listed, passed}
  ' "$1"
}
loop_nextest_run_doctests() {
  local work=$1 inventory=$2 all ignored _unused
  shift 2
  local -a skip=(--exact)
  while IFS= read -r name; do skip+=(--skip "$name"); done < <(
    jq -r '."rust-suites"[].testcases | keys[]' "$inventory" | LC_ALL=C sort -u
  )
  CARGO_TERM_COLOR=never cargo test --manifest-path research-core/Cargo.toml --locked "$@" \
    -- --list >"$work/doctests-list.txt" 2>&1 || { cat "$work/doctests-list.txt" >&2; return 1; }
  CARGO_TERM_COLOR=never cargo test --manifest-path research-core/Cargo.toml --locked "$@" \
    -- --ignored --list >"$work/doctests-ignored.txt" 2>&1 || { cat "$work/doctests-ignored.txt" >&2; return 1; }
  read -r all _unused < <(loop_nextest_doc_counts "$work/doctests-list.txt")
  read -r ignored _unused < <(loop_nextest_doc_counts "$work/doctests-ignored.txt")
  LOOP_DOC_LISTED=$((all - ignored))
  (( LOOP_DOC_LISTED >= 0 )) || return 1
  CARGO_TERM_COLOR=never cargo test --manifest-path research-core/Cargo.toml --locked "$@" \
    -- "${skip[@]}" >"$work/doctests-run.txt" 2>&1 || { cat "$work/doctests-run.txt" >&2; return 1; }
  cat "$work/doctests-run.txt"
  read -r _unused LOOP_DOC_PASSED < <(loop_nextest_doc_counts "$work/doctests-run.txt")
  [[ $LOOP_DOC_PASSED == "$LOOP_DOC_LISTED" ]] || {
    printf 'doctests passed %s != discovered runnable %s\n' "$LOOP_DOC_PASSED" "$LOOP_DOC_LISTED" >&2
    return 1
  }
}
