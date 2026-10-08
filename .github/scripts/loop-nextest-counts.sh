#!/usr/bin/env bash
# Counters and exact test identities for four balanced archive partitions.
# Source this file. Do not execute it.

loop_nextest_stderr_recompiled() {
  local file=$1 cleaned
  cleaned=$(sed $'s/\033\\[[0-9;]*[A-Za-z]//g' "$file")
  grep -Eq '^[[:space:]]*Compiling [^[:space:]]+ v[0-9]' <<<"$cleaned"
}

# Runnable tests in a `cargo nextest list --message-format json` document.
# Ignored tests and partition mismatches are not runnable.
loop_nextest_binaries_from_list() {
  local list_json=$1
  jq -e -c '
    [
      (."rust-suites" // error("nextest list has no rust-suites"))
      | to_entries[].value
      | select((.status == null) or .status == "listed")
      | . as $suite
      | ($suite.testcases // {})
      | to_entries[].value
      | select((.kind // "test") == "test")
      | select(.ignored != true)
      | select(.["filter-match"].status == "matches")
      | {key: "\($suite["package-name"])::\($suite["binary-name"]) (\($suite.kind))", n: 1}
    ]
    | group_by(.key)
    | map({key: .[0].key, value: (map(.n) | add)})
    | from_entries
  ' "$list_json"
}

loop_nextest_tests_from_list() {
  jq -ec '[."rust-suites" | to_entries[] | . as $suite
    | select((.value.status == null) or .value.status == "listed")
    | .value.testcases | to_entries[]
    | select(.value.ignored != true and .value["filter-match"].status == "matches")
    | [$suite.key, .key]] | sort' "$1"
}

loop_nextest_write_expected() {
  local list_json=$1 output=$2 build_seconds=$3 doctests_listed=$4 doctests_passed=$5
  local binaries runnable alpha
  binaries=$(loop_nextest_binaries_from_list "$list_json")
  runnable=$(jq -e 'add // 0' <<<"$binaries")
  alpha=$(jq -r '.["alpha-harness::alpha_harness (lib)"] // 0' <<<"$binaries")
  [[ $runnable =~ ^[0-9]+$ ]] || {
    printf 'invalid loop archive runnable count\n' >&2
    return 1
  }
  jq -en \
    --argjson binaries "$binaries" \
    --argjson runnable "$runnable" \
    --argjson alpha "$alpha" \
    --argjson build_seconds "$build_seconds" \
    --argjson doctests_listed "$doctests_listed" \
    --argjson doctests_passed "$doctests_passed" \
    '{
      build_seconds: $build_seconds,
      nextest_runnable: $runnable,
      alpha_harness: $alpha,
      binaries: $binaries,
      doctests_listed: $doctests_listed,
      doctests_passed: $doctests_passed,
      partition: "hash",
      shards: 4
    }' >"$output"
  printf 'LOOP_NEXTEST_BUILD_SECONDS=%s\n' "$build_seconds"
  printf 'LOOP_NEXTEST_RUNNABLE=%s\n' "$runnable"
  printf 'LOOP_NEXTEST_ALPHA_HARNESS=%s\n' "$alpha"
}

# Compare one shard's libtest-json-plus event stream with that partition's list.
loop_nextest_write_shard() {
  local events=$1 list_json=$2 output=$3 shard=$4 seconds=$5 run_status=$6 recompiled=$7
  local executed listed failed_tests tests mode=${8:-balanced-v1}
  [[ $mode == hash || $mode == balanced-v1 ]] || return 1
  executed=$(jq -s -e -c '
    def rows:
      [.[]
        | select(.type == "suite" and (.event == "ok" or .event == "failed"))
        | select((.passed + .failed) > 0)
        | {
            key: "\(.nextest.crate)::\(.nextest.test_binary) (\(.nextest.kind))",
            passed,
            failed
          }];
    (rows | group_by(.key) | map({
      key: .[0].key,
      passed: (map(.passed) | add),
      failed: (map(.failed) | add)
    })) as $rows
    | (map(select(.type == "test" and .event == "ok")) | length) as $test_ok
    | (map(select(.type == "test" and .event == "failed")) | length) as $test_failed
    | ($rows | map(.passed) | add // 0) as $passed
    | ($rows | map(.failed) | add // 0) as $failed
    | if $test_ok != $passed or $test_failed != $failed then
        error("event totals \($test_ok)/\($test_failed) != suite totals \($passed)/\($failed)")
      else
        {
          passed: $passed,
          failed: $failed,
          binaries: ($rows | map({key, value: (.passed + .failed)}) | from_entries)
        }
      end
  ' "$events")
  # Counts alone cannot detect one omitted test replaced by a duplicate.
  # Compare terminal event names with every selected test before writing evidence.
  if ! jq -en --slurpfile events <(jq -s . "$events") --slurpfile list "$list_json" '
    ([$list[0]."rust-suites" | to_entries[].value | . as $suite
      | select(.status == null or .status == "listed")
      | .testcases | to_entries[]
      | select(.value.ignored != true and .value["filter-match"].status == "matches")
      | "\($suite["package-name"])::\($suite["binary-name"])$\(.key)"] | sort) as $want
    | ([$events[0][] | select(.type == "test" and (.event == "ok" or .event == "failed")) | .name] | sort) as $got
    | $want == $got and ($want | unique | length) == ($want | length)
  ' >/dev/null; then
    printf 'shard %s test identities differ from its list\n' "$shard" >&2
    return 1
  fi
  tests=$(loop_nextest_tests_from_list "$list_json")
  failed_tests=$(jq -s -c '[.[] | select(.type == "test" and .event == "failed") | .name]' "$events")
  listed=$(loop_nextest_binaries_from_list "$list_json")
  if ! jq -e -n --argjson executed "$executed" --argjson listed "$listed" \
    '$executed.binaries == $listed' >/dev/null; then
    printf 'shard %s ran a different set than its partition list\n' "$shard" >&2
    printf 'listed=%s\nexecuted=%s\n' "$listed" "$(jq -c '.binaries' <<<"$executed")" >&2
    return 1
  fi
  jq -en \
    --argjson shard "$shard" \
    --argjson seconds "$seconds" \
    --argjson status "$run_status" \
    --argjson recompiled "$recompiled" \
    --argjson executed "$executed" \
    --argjson failed_tests "$failed_tests" \
    --argjson tests "$tests" \
    --arg mode "$mode" \
    '{
      shard: $shard,
      partition: ($mode + ":" + ($shard | tostring) + "/4"),
      seconds: $seconds,
      exit_status: $status,
      recompiled: $recompiled,
      passed: $executed.passed,
      failed: $executed.failed,
      binaries: $executed.binaries,
      failed_tests: $failed_tests,
      tests: $tests
    }' >"$output"
  printf 'LOOP_NEXTEST_SHARD=%s\n' "$shard"
  printf 'LOOP_NEXTEST_SHARD_SECONDS=%s\n' "$seconds"
  printf 'LOOP_NEXTEST_SHARD_PASSED=%s\n' "$(jq -r '.passed' <<<"$executed")"
  printf 'LOOP_NEXTEST_SHARD_FAILED=%s\n' "$(jq -r '.failed' <<<"$executed")"
  printf 'LOOP_NEXTEST_SHARD_RECOMPILED=%s\n' "$recompiled"
  if [[ $(jq -r 'length' <<<"$failed_tests") -gt 0 ]]; then
    printf 'LOOP_NEXTEST_SHARD_FAILED_TESTS=%s\n' "$(jq -r 'join(",")' <<<"$failed_tests")"
  fi
}

loop_nextest_verify_dir() {
  local dir=$1 shard_n shard passed failed recompiled exit_status partition
  local expected_runnable got_passed alpha expected_alpha mode
  local -a errors=()
  [[ -f $dir/expected-counts.json ]] || { printf 'missing expected-counts.json\n' >&2; return 1; }
  expected_runnable=$(jq -r '.nextest_runnable' "$dir/expected-counts.json")
  alpha=$(jq -r '.alpha_harness' "$dir/expected-counts.json")
  mode=$(jq -r '.partition' "$dir/expected-counts.json")
  [[ $mode == hash || $mode == balanced-v1 ]] || errors+=("invalid partition mode")
  [[ $(jq -r '.mode' "$dir/expected-counts.json") == "$mode" ]] || errors+=("partition mode label mismatch")
  [[ $(jq -r '.shards' "$dir/expected-counts.json") == 4 ]] || errors+=("expected shard count is not 4")
  [[ $(jq -r '.doctests_listed' "$dir/expected-counts.json") == "$(jq -r '.doctests_passed' "$dir/expected-counts.json")" ]] \
    || errors+=("doctest list does not match doctest passes")
  [[ $expected_runnable =~ ^[0-9]+$ ]] || errors+=("expected runnable count is empty")
  got_passed=0
  for shard_n in 1 2 3 4; do
    if [[ ! -f $dir/shard-$shard_n.json ]]; then
      errors+=("shard $shard_n report is missing")
      continue
    fi
    shard=$(cat "$dir/shard-$shard_n.json")
    passed=$(jq -r '.passed' <<<"$shard")
    failed=$(jq -r '.failed' <<<"$shard")
    recompiled=$(jq -r '.recompiled' <<<"$shard")
    exit_status=$(jq -r '.exit_status' <<<"$shard")
    partition=$(jq -r '.partition' <<<"$shard")
    [[ $(jq -r '.shard' <<<"$shard") == "$shard_n" && $partition == "$mode:$shard_n/4" ]] \
      || errors+=("shard $shard_n partition is $partition")
    [[ $recompiled == false ]] || errors+=("shard $shard_n recompiled")
    [[ $exit_status == 0 && $failed == 0 ]] || errors+=("shard $shard_n exit=$exit_status failed=$failed")
    [[ $passed == "$(jq -r '[.binaries[]] | add // 0' <<<"$shard")" ]] \
      || errors+=("shard $shard_n passed does not match its binaries")
    [[ $(jq -r '.failed_tests | length' <<<"$shard") == 0 ]] \
      || errors+=("shard $shard_n failed tests: $(jq -r '.failed_tests | join(", ")' <<<"$shard")")
    got_passed=$((got_passed + passed))
  done
  if [[ ${#errors[@]} -gt 0 ]]; then
    printf 'loop nextest counts do not match:\n' >&2
    printf '%s\n' "${errors[@]}" >&2
    return 1
  fi
  [[ $got_passed == "$expected_runnable" ]] || errors+=("passed $got_passed != listed $expected_runnable")
  if ! jq -e -n \
    --slurpfile expected "$dir/expected-counts.json" \
    --slurpfile s1 "$dir/shard-1.json" \
    --slurpfile s2 "$dir/shard-2.json" \
    --slurpfile s3 "$dir/shard-3.json" \
    --slurpfile s4 "$dir/shard-4.json" '
      ($expected[0].binaries) as $want
      | (reduce ([$s1[0], $s2[0], $s3[0], $s4[0]][]) as $shard ({};
          reduce ($shard.binaries | to_entries[]) as $bin (.; .[$bin.key] += $bin.value))) as $got
      | $got == $want
        and (($want["alpha-harness::alpha_harness (lib)"] // 0) == $expected[0].alpha_harness)
    ' >/dev/null; then
    expected_alpha=$(jq -r '.binaries["alpha-harness::alpha_harness (lib)"] // 0' "$dir/expected-counts.json")
    errors+=("per-binary counts != archive list (alpha_harness field $alpha, list $expected_alpha)")
  fi
  if [[ ${#errors[@]} -gt 0 ]]; then
    printf 'loop nextest counts do not match:\n' >&2
    printf '%s\n' "${errors[@]}" >&2
    return 1
  fi
  if ! jq -en --slurpfile expected "$dir/expected-counts.json" \
    --slurpfile s1 "$dir/shard-1.json" --slurpfile s2 "$dir/shard-2.json" \
    --slurpfile s3 "$dir/shard-3.json" --slurpfile s4 "$dir/shard-4.json" '
    [$s1[0].tests, $s2[0].tests, $s3[0].tests, $s4[0].tests] as $sets
    | ($sets | add | sort) as $got
    | ($expected[0].tests | sort) as $want
    | ($want | type) == "array" and ($want | length) == $expected[0].nextest_runnable
      and ($got | unique | length) == ($got | length)
      and $got == $want and $sets == $expected[0].shard_tests
  ' >/dev/null; then
    printf 'loop nextest test identities overlap, omit tests, or drift from the plan\n' >&2
    return 1
  fi
  printf 'LOOP_NEXTEST_RUNNABLE=%s\n' "$expected_runnable"
  printf 'LOOP_NEXTEST_ALPHA_HARNESS=%s\n' "$alpha"
  printf 'LOOP_NEXTEST_BUILD_SECONDS=%s\n' "$(jq -r '.build_seconds' "$dir/expected-counts.json")"
  printf 'LOOP_NEXTEST_SHARD_PASSED=%s\n' "$(jq -nr \
    --slurpfile s1 "$dir/shard-1.json" --slurpfile s2 "$dir/shard-2.json" \
    --slurpfile s3 "$dir/shard-3.json" --slurpfile s4 "$dir/shard-4.json" \
    '[$s1[0].passed, $s2[0].passed, $s3[0].passed, $s4[0].passed] | join(",")')"
  printf 'LOOP_NEXTEST_SHARD_SECONDS=%s\n' "$(jq -nr \
    --slurpfile s1 "$dir/shard-1.json" --slurpfile s2 "$dir/shard-2.json" \
    --slurpfile s3 "$dir/shard-3.json" --slurpfile s4 "$dir/shard-4.json" \
    '[$s1[0].seconds, $s2[0].seconds, $s3[0].seconds, $s4[0].seconds] | join(",")')"
}
