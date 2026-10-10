#!/usr/bin/env bash
# Exercise image readback and startup with CLI fixtures; no registry or compute writes.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin" "$work/producer" "$work/image" "$work/containers"
export IMAGE_CONTENT="$work/image" IMAGE_SHA=1111111111111111111111111111111111111111
export EXPECTED_IMAGE=fixture@sha256:1111111111111111111111111111111111111111111111111111111111111111
export MOCK_RUN_LOG="$work/runs" MOCK_DEADLINE_LOG="$work/deadlines"
export MOCK_CIDFILE_LOG="$work/cidfiles" MOCK_REMOVE_LOG="$work/removals"
export MOCK_CONTAINERS="$work/containers"
export MOCK_FAIL_BINARY='' MOCK_FAIL_CODE=37 MOCK_TIMEOUT=false

cat >"$work/bin/docker" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  image)
    [[ $# == 5 && $2 == inspect && $3 == --format &&
       $4 == '{{ index .Config.Labels "org.opencontainers.image.revision" }}' &&
       $5 == "$EXPECTED_IMAGE" ]]
    printf '%s\n' "$IMAGE_SHA"
    ;;
  create)
    [[ $# == 2 && $2 == "$EXPECTED_IMAGE" ]]
    touch "$MOCK_CONTAINERS/fixture-container"
    printf 'fixture-container\n'
    ;;
  cp)
    [[ $# == 3 && $2 == 'fixture-container:/usr/local/bin/.' && -d $3 ]]
    cp -R "$IMAGE_CONTENT/." "$3/"
    ;;
  run)
    [[ $# == 10 && $2 == --rm && $3 == --network && $4 == none && $5 == --cidfile ]]
    cidfile=$6
    test ! -e "$cidfile"
    shift 6
    [[ $1 == --entrypoint && $3 == "$EXPECTED_IMAGE" && $4 == --help ]]
    binary=$(basename "$2")
    [[ $2 == "/usr/local/bin/$binary" && -f "$IMAGE_CONTENT/$binary" ]]
    container="fixture-smoke-$binary"
    printf '%s\n' "$container" >"$cidfile"
    printf '%s\n' "$cidfile" >>"$MOCK_CIDFILE_LOG"
    touch "$MOCK_CONTAINERS/$container"
    printf '%s\n' "$binary" >>"$MOCK_RUN_LOG"
    # Model an attached container left behind when its Docker client times out.
    if [[ $MOCK_TIMEOUT == true ]]; then exit 0; fi
    if [[ $MOCK_FAIL_BINARY == "$binary" ]]; then exit "$MOCK_FAIL_CODE"; fi
    rm "$MOCK_CONTAINERS/$container"
    ;;
  rm)
    [[ $# == 3 && $2 == -f ]]
    case "$3" in fixture-container|fixture-smoke-*) ;; *) exit 92 ;; esac
    printf '%s\n' "$3" >>"$MOCK_REMOVE_LOG"
    rm -f "$MOCK_CONTAINERS/$3"
    ;;
  *) exit 91 ;;
esac
MOCK

cat >"$work/bin/timeout" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
[[ $# -ge 3 && $1 == --kill-after=5s && $2 == 30s && $3 == docker ]]
printf '%s %s\n' "$1" "$2" >>"$MOCK_DEADLINE_LOG"
shift 2
if [[ $MOCK_TIMEOUT == true ]]; then
  "$@"
  exit 124
fi
exec "$@"
MOCK
chmod 0755 "$work/bin/docker" "$work/bin/timeout"
export PATH="$work/bin:$PATH"

reset_probe_evidence() {
  : >"$MOCK_RUN_LOG"
  : >"$MOCK_DEADLINE_LOG"
  : >"$MOCK_CIDFILE_LOG"
  : >"$MOCK_REMOVE_LOG"
  rm -f "$work/proof-issued" "$work/publication-complete"
}

assert_cleanup() {
  test -z "$(find "$MOCK_CONTAINERS" -mindepth 1 -print -quit)"
  while IFS= read -r cidfile; do test ! -e "$cidfile"; done <"$MOCK_CIDFILE_LOG"
}

verifier="$root/.github/scripts/verify-research-product-image.sh"
verify_publication() {
  # Model downstream markers after successful readback, without publishing.
  bash -eu -c '
    bash "$1" "$2" "$3" "$4" "$5"
    touch "$6/proof-issued" "$6/publication-complete"
  ' -- "$verifier" "$EXPECTED_IMAGE" "${2:-$IMAGE_SHA}" "$work/producer" "$1" "$work"
}

assert_no_publication() {
  test ! -e "$work/proof-issued"
  test ! -e "$work/publication-complete"
}

for product in cex-runner prediction-runner; do
  rm -f "$work/image/"* "$work/producer/"*
  expected=$(bash "$root/.github/scripts/research-release-products.sh" binaries "$product")
  printf '%s\n' "$expected" >"$work/expected-runs"
  while IFS= read -r binary; do
    printf 'verified fixture: %s\n' "$binary" >"$work/producer/$binary"
    chmod 0755 "$work/producer/$binary"
    cp -p "$work/producer/$binary" "$work/image/$binary"
  done <<<"$expected"

  reset_probe_evidence
  verify_publication "$product"
  diff -u "$work/expected-runs" "$MOCK_RUN_LOG"
  test "$(wc -l <"$MOCK_DEADLINE_LOG")" -eq "$(wc -l <"$work/expected-runs")"
  test -f "$work/proof-issued"
  test -f "$work/publication-complete"
  assert_cleanup

  for failure in first middle; do
    if [[ $failure == first ]]; then
      MOCK_FAIL_BINARY=$(sed -n '1p' "$work/expected-runs")
      MOCK_FAIL_CODE=37
      head -n 1 "$work/expected-runs" >"$work/expected-prefix"
    else
      MOCK_FAIL_BINARY=$(sed -n '3p' "$work/expected-runs")
      MOCK_FAIL_CODE=127
      head -n 3 "$work/expected-runs" >"$work/expected-prefix"
    fi
    test -n "$MOCK_FAIL_BINARY"
    reset_probe_evidence
    status=0
    verify_publication "$product" >"$work/rejection" 2>&1 || status=$?
    test "$status" -eq "$MOCK_FAIL_CODE"
    diff -u "$work/expected-prefix" "$MOCK_RUN_LOG"
    grep -Fxq "fixture-smoke-$MOCK_FAIL_BINARY" "$MOCK_REMOVE_LOG"
    assert_cleanup
    assert_no_publication
  done

  # Reject every admitted program independently and preserve its failure code.
  : >"$work/expected-prefix"
  while IFS= read -r binary; do
    MOCK_FAIL_BINARY=$binary
    MOCK_FAIL_CODE=73
    printf '%s\n' "$binary" >>"$work/expected-prefix"
    reset_probe_evidence
    status=0
    verify_publication "$product" >"$work/rejection" 2>&1 || status=$?
    test "$status" -eq 73
    diff -u "$work/expected-prefix" "$MOCK_RUN_LOG"
    grep -Fxq "fixture-smoke-$binary" "$MOCK_REMOVE_LOG"
    assert_cleanup
    assert_no_publication
    printf 'PASS: startup rejection blocks publication: product=%s binary=%s exit=%s\n' "$product" "$binary" "$status"
  done <"$work/expected-runs"
  MOCK_FAIL_BINARY=

  reset_probe_evidence
  MOCK_TIMEOUT=true
  status=0
  verify_publication "$product" >"$work/rejection" 2>&1 || status=$?
  test "$status" -eq 124
  head -n 1 "$work/expected-runs" >"$work/expected-prefix"
  diff -u "$work/expected-prefix" "$MOCK_RUN_LOG"
  grep -Fxq "fixture-smoke-$(head -n 1 "$work/expected-runs")" "$MOCK_REMOVE_LOG"
  assert_cleanup
  assert_no_publication
  MOCK_TIMEOUT=false

  if [[ $product == cex-runner ]]; then
    foreign=monday-prediction-evaluator
    own=hft-backtest
  else
    foreign=alpha-harness
    own=monday-prediction-research
  fi
  for invalid in foreign tampered source missing nonexecutable cid_injection; do
    reset_probe_evidence
    source=$IMAGE_SHA
    case "$invalid" in
      foreign) printf 'foreign executable\n' >"$work/image/$foreign" ;;
      tampered) printf 'tampered\n' >>"$work/image/$own" ;;
      source) source=2222222222222222222222222222222222222222 ;;
      missing) rm "$work/image/$own" ;;
      nonexecutable) chmod 0644 "$work/image/$own" ;;
      cid_injection)
        printf 'fixture-smoke-protected\n' >"$work/image/smoke.cid"
        touch "$MOCK_CONTAINERS/fixture-smoke-protected"
        ;;
    esac
    status=0
    verify_publication "$product" "$source" >"$work/rejection" 2>&1 || status=$?
    if [[ $status == 0 ]]; then echo "invalid image accepted: $product/$invalid" >&2; exit 1; fi
    test ! -s "$MOCK_RUN_LOG"
    test ! -s "$MOCK_DEADLINE_LOG"
    if [[ $invalid == cid_injection ]]; then
      test -f "$MOCK_CONTAINERS/fixture-smoke-protected"
      if grep -Fxq fixture-smoke-protected "$MOCK_REMOVE_LOG"; then
        echo 'image bytes were trusted as a cleanup container ID' >&2
        exit 1
      fi
      rm "$MOCK_CONTAINERS/fixture-smoke-protected"
    fi
    assert_cleanup
    assert_no_publication
    rm -f "$work/image/$foreign" "$work/image/smoke.cid"
    cp -p "$work/producer/$own" "$work/image/$own"
  done
done
printf 'PASS: exact product startup, offline arguments, startup failure and timeout cleanup; invalid bytes never execute or select cleanup targets\n'
printf 'PASS: every runner startup failure blocks downstream proof and publication markers\n'
