#!/usr/bin/env bash
set -euo pipefail
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../../.." && pwd)"
epic_sync="$repo_root/.claude/commands/pm/epic-sync.md"
init="$repo_root/.claude/scripts/pm/init.sh"
extract_bash_after() {
  local file="$1"
  local heading="$2"
  awk -v heading="$heading" '
    $0 == heading { found=1; next }
    found && /^```bash$/ { in_block=1; next }
    in_block && /^```$/ { exit }
    in_block { print }
  ' "$file"
}
extract_bash_section() {
  local file="$1"
  local heading="$2"
  awk -v heading="$heading" '
    $0 == heading { found=1; next }
    found && /^### / { exit }
    found && /^```bash$/ { in_block=1; next }
    found && in_block && /^```$/ { in_block=0; next }
    found && in_block { print }
  ' "$file"
}
write_publication_script() {
  local target="$1"
  shift
  {
    printf '#!/usr/bin/env bash\nset -euo pipefail\nARGUMENTS=feature\n'
    for heading in "$@"; do
      extract_bash_after "$epic_sync" "$heading"
    done
    # The generated script, not this test, expands these variables.
    # shellcheck disable=SC2016
    printf 'printf "%%s\\n" "$MONDAY_EPIC_SYNC_TMP" > "$RUN_ROOT_OUTPUT"\n'
  } > "$target"
  chmod +x "$target"
}
write_section_script() {
  local target="$1"
  local setup="$2"
  local heading="$3"
  {
    printf '#!/usr/bin/env bash\nset -euo pipefail\nARGUMENTS=feature\n'
    printf '%s\n' "$setup"
    extract_bash_after "$epic_sync" "$heading"
  } > "$target"
  chmod +x "$target"
}
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/project" "$scratch/run-parent"
write_publication_script "$scratch/quick-check.sh" "## Quick Check"
if (
  cd "$scratch/project"
  RUN_ROOT_OUTPUT="$scratch/missing-root" \
    MONDAY_EPIC_SYNC_TMP_PARENT="$scratch/run-parent" \
    bash "$scratch/quick-check.sh"
) > /dev/null 2>&1; then
  echo "epic publication accepted a missing local epic mirror" >&2
  exit 1
fi
mkdir -p "$scratch/project/.claude/epics/feature"
cat > "$scratch/project/.claude/epics/feature/epic.md" <<'EOF'
---
name: feature
---
# Feature
EOF
cat > "$scratch/project/.claude/epics/feature/001.md" <<'EOF'
---
name: First task
depends_on: []
---
# First task
EOF
for run in 1 2; do
  (
    cd "$scratch/project"
    RUN_ROOT_OUTPUT="$scratch/run-$run" \
      MONDAY_EPIC_SYNC_TMP="$scratch/stale-root" \
      MONDAY_EPIC_SYNC_TMP_PARENT="$scratch/run-parent" \
      bash "$scratch/quick-check.sh"
  ) > /dev/null
done
first_root="$(cat "$scratch/run-1")"
second_root="$(cat "$scratch/run-2")"
if [ "$first_root" = "$second_root" ] ||
  [ "$first_root" = "$scratch/stale-root" ] ||
  [ "$second_root" = "$scratch/stale-root" ] ||
  [ -e "$first_root" ] || [ -e "$second_root" ]; then
  echo "epic publication reused or retained a scratch root" >&2
  exit 1
fi
if grep -Eq 'gh(-| )sub-issue' "$epic_sync" "$init"; then
  echo "native publication still requires the legacy extension" >&2
  exit 1
fi
if grep -Fq -- '--limit 1000' "$epic_sync"; then
  echo "resume lookup still has a fixed issue-count ceiling" >&2
  exit 1
fi

mkdir -p "$scratch/bin" "$scratch/gh-state"
gh_log="$scratch/gh.log"
cat > "$scratch/bin/gh" <<'EOF'
#!/usr/bin/env bash
set -u
: "${GH_LOG:?}"
printf '%s\n' "$*" >> "$GH_LOG"
state="${GH_STATE_DIR:?}"

case "$*" in
  "--version") echo "gh version 2.95.0 (test)"; exit 0 ;;
  "auth status") echo "Logged in to github.com"; exit 0 ;;
  "issue create --help") [ "${GH_MISSING_NATIVE:-0}" = 1 ] || echo "--parent"; exit 0 ;;
  "issue edit --help") echo "--add-blocked-by"; exit 0 ;;
  "api --paginate"*"task:001"*)
    [ ! -f "$state/501.body" ] || echo "https://github.com/example/repo/issues/501"
    exit 0
    ;;
  "api --paginate"*"task:002"*)
    [ ! -f "$state/502.body" ] || echo "https://github.com/example/repo/issues/502"
    exit 0
    ;;
  "api --paginate"*"epic:feature"*)
    [ ! -f "$state/500.body" ] || echo "https://github.com/example/repo/issues/500"
    exit 0
    ;;
  "repo view --json nameWithOwner -q .nameWithOwner")
    echo "example/repo"
    exit 0
    ;;
esac

if [ "$1 $2" = "issue create" ]; then
  shift 2
  body_file=""
  parent=""
  labels=""
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --body-file) body_file="$2"; shift 2 ;;
      --parent) parent="$2"; shift 2 ;;
      --label) labels="$2"; shift 2 ;;
      *) shift ;;
    esac
  done

  if grep -Fq 'monday-source: epic:feature/task:002' "$body_file"; then
    if [ ! -f "$state/task-002-failed-once" ]; then
      : > "$state/task-002-failed-once"
      exit 1
    fi
    number=502
  elif grep -Fq 'monday-source: epic:feature/task:001' "$body_file"; then
    number=501
  else
    number=500
  fi
  cp "$body_file" "$state/$number.body"
  printf '%s\n' "$labels" | tr ',' '\n' > "$state/$number.labels"
  printf '%s\n' "$parent" > "$state/$number.parent"
  echo "https://github.com/example/repo/issues/$number"
  exit 0
fi

if [ "$1 $2" = "issue view" ]; then
  number="$3"
  case "$*" in
    *"--json body --jq .body") cat "$state/$number.body" ;;
    *"--json labels --jq .labels[].name") cat "$state/$number.labels" ;;
    *"--json parent --jq .parent.number") cat "$state/$number.parent" ;;
    *"--json subIssues --jq .subIssues.nodes[].number")
      [ ! -f "$state/501.body" ] || echo 501
      [ ! -f "$state/502.body" ] || echo 502
      ;;
    *"--json blockedBy --jq .blockedBy.nodes[].number")
      [ ! -f "$state/$number.blockers" ] || cat "$state/$number.blockers"
      ;;
    *) echo "unexpected issue view: $*" >&2; exit 1 ;;
  esac
  exit 0
fi

if [ "$1 $2" = "issue edit" ]; then
  number="$3"
  blocker="$5"
  printf '%s\n' "$blocker" >> "$state/$number.blockers"
  exit 0
fi

echo "unexpected gh invocation: $*" >&2
exit 1
EOF
chmod +x "$scratch/bin/gh"
cat > "$scratch/bin/mv" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [ "${MV_FAIL_INSTALL:-0}" = 1 ]; then
  case "$1" in
    */rename-stage/501.md) exit 1 ;;
  esac
fi
exec /bin/mv "$@"
EOF
chmod +x "$scratch/bin/mv"

cat > "$scratch/project/.claude/epics/feature/002.md" <<'EOF'
---
name: Second task
depends_on: [001]
---
# Second task
EOF

publication_headings=(
  "## Quick Check"
  "## Native Relationship Preflight"
  "### 1. Create or Resume Epic Issue"
  "### 2. Create or Resume Native Task Sub-Issues"
  "### 2b. Validate Complete Mapping"
  "### 2c. Verify Native Publications"
  "### 3. Publish and Verify Native Dependencies"
)
write_publication_script "$scratch/publish.sh" "${publication_headings[@]}"

run_publication() (
  cd "$scratch/project"
  GH_LOG="$gh_log" GH_STATE_DIR="$scratch/gh-state" \
    MONDAY_EPIC_SYNC_TMP_PARENT="$scratch/run-parent" \
    PATH="$scratch/bin:$PATH" RUN_ROOT_OUTPUT="$1" \
    bash "$scratch/publish.sh"
)

: > "$gh_log"
if run_publication "$scratch/partial-root" > /dev/null 2>&1; then
  echo "partial publication did not surface a child creation failure" >&2
  exit 1
fi
if [ -n "$(find "$scratch/run-parent" -mindepth 1 -print -quit)" ]; then
  echo "failed publication retained its scratch root" >&2
  exit 1
fi
sed '1a\
monday_source: 001
' "$scratch/project/.claude/epics/feature/001.md" > \
  "$scratch/project/.claude/epics/feature/501.md"
rm "$scratch/project/.claude/epics/feature/001.md"
printf '%s\n' enhancement ready-for-agent tracking > "$scratch/gh-state/500.labels"
printf '%s\n' enhancement ready-for-agent > "$scratch/gh-state/501.labels"
run_publication "$scratch/resumed-root" > /dev/null

if [ "$(grep -c '^issue create --title Epic:' "$gh_log")" -ne 1 ] ||
  [ "$(grep -c '^issue create --parent 500' "$gh_log")" -ne 3 ] ||
  [ "$(grep -c 'monday-source: epic:feature/task:001' "$scratch/gh-state/501.body")" -ne 1 ] ||
  [ ! -f "$scratch/gh-state/502.body" ]; then
  echo "partial publication retry created duplicates or lost a child" >&2
  exit 1
fi
grep -Fq "api --paginate repos/{owner}/{repo}/issues?state=all&per_page=100" "$gh_log"
for readback in "500 --json subIssues" "501 --json labels" \
  "502 --json parent" "502 --json blockedBy"; do
  grep -Fq "issue view $readback" "$gh_log"
done
if [ "$(cat "$scratch/gh-state/502.blockers")" != 501 ]; then
  echo "native blocked-by relationship was not published" >&2
  exit 1
fi

rename_project="$scratch/rename-project"
rename_root="$scratch/rename-root"
mkdir -p "$rename_project/.claude/epics/feature" "$rename_root"
cat > "$rename_project/.claude/epics/feature/epic.md" <<'EOF'
---
name: feature
---
# Feature
EOF
cat > "$rename_project/.claude/epics/feature/001.md" <<'EOF'
---
name: First original
github:
updated:
---
# First original
EOF
cat > "$rename_project/.claude/epics/feature/002.md" <<'EOF'
---
name: Second original
github:
updated:
---
# Second original
EOF
{
  printf '#!/usr/bin/env bash\nset -euo pipefail\nARGUMENTS=feature\n'
  extract_bash_after "$epic_sync" "## Quick Check"
  cat <<'EOF'
cat > "$epic_sync_tmp/task-mapping.txt" <<'MAPPINGS'
.claude/epics/feature/001.md:002
.claude/epics/feature/002.md:501
MAPPINGS
EOF
  extract_bash_section "$epic_sync" "### 4. Rename Task Files and Update References"
} > "$scratch/rename.sh"
chmod +x "$scratch/rename.sh"
(
  cd "$rename_project"
  GH_LOG="$gh_log" GH_STATE_DIR="$scratch/gh-state" \
    PATH="$scratch/bin:$PATH" MONDAY_EPIC_SYNC_TMP_PARENT="$rename_root" \
    bash "$scratch/rename.sh"
) > /dev/null
if [ -e "$rename_project/.claude/epics/feature/001.md" ] ||
  ! grep -Fq '# First original' "$rename_project/.claude/epics/feature/002.md" ||
  ! grep -Fq 'monday_source: 001' "$rename_project/.claude/epics/feature/002.md" ||
  ! grep -Fq '# Second original' "$rename_project/.claude/epics/feature/501.md" ||
  ! grep -Fq 'monday_source: 002' "$rename_project/.claude/epics/feature/501.md"; then
  echo "task renaming overwrote a pending source task" >&2
  exit 1
fi

failure_project="$scratch/rename-failure-project"
failure_root="$scratch/rename-failure-root"
mkdir -p "$failure_project/.claude/epics/feature" "$failure_root"
cp "$rename_project/.claude/epics/feature/epic.md" \
  "$failure_project/.claude/epics/feature/epic.md"
cp "$rename_project/.claude/epics/feature/002.md" \
  "$failure_project/.claude/epics/feature/001.md"
cp "$rename_project/.claude/epics/feature/501.md" \
  "$failure_project/.claude/epics/feature/002.md"
if (
  cd "$failure_project"
  GH_LOG="$gh_log" GH_STATE_DIR="$scratch/gh-state" MV_FAIL_INSTALL=1 \
    PATH="$scratch/bin:$PATH" MONDAY_EPIC_SYNC_TMP_PARENT="$failure_root" \
    bash "$scratch/rename.sh"
) > /dev/null 2>&1; then
  echo "task rename did not surface an install failure" >&2
  exit 1
fi
if ! grep -Fq '# First original' "$failure_project/.claude/epics/feature/001.md" ||
  ! grep -Fq '# Second original' "$failure_project/.claude/epics/feature/002.md" ||
  [ -e "$failure_project/.claude/epics/feature/501.md" ] ||
  find "$failure_project/.claude/epics" -maxdepth 1 \
    -name '.feature-rename-recovery.*' -print -quit | grep -q .; then
  echo "failed task rename did not restore the source files" >&2
  exit 1
fi

# The generated section script expands the scratch-root expression.
# shellcheck disable=SC2016
mapping_setup='epic_dir=.claude/epics/feature
task_count=2
epic_sync_tmp="${MONDAY_EPIC_SYNC_TMP:?}"'
write_section_script "$scratch/validate-mapping.sh" "$mapping_setup" \
  "### 2b. Validate Complete Mapping"

for mapping_case in incomplete duplicate; do
  mapping_root="$scratch/mapping-$mapping_case"
  mkdir -p "$mapping_root"
  printf '%s\n' '.claude/epics/feature/501.md:501' > "$mapping_root/task-mapping.txt"
  if [ "$mapping_case" = duplicate ]; then
    printf '%s\n' '.claude/epics/feature/002.md:501' >> "$mapping_root/task-mapping.txt"
  fi
  if (
    cd "$scratch/project"
    MONDAY_EPIC_SYNC_TMP="$mapping_root" bash "$scratch/validate-mapping.sh"
  ) > /dev/null 2>&1; then
    echo "publication accepted $mapping_case task mapping" >&2
    exit 1
  fi
done

mkdir -p "$scratch/init-project"
if (
  cd "$scratch/init-project"
  GH_MISSING_NATIVE=1 GH_LOG="$gh_log" GH_STATE_DIR="$scratch/gh-state" \
    PATH="$scratch/bin:$PATH" bash "$init"
) > /dev/null 2>&1; then
  echo "init accepted a GitHub CLI without native parent support" >&2
  exit 1
fi
(
  cd "$scratch/init-project"
  GH_LOG="$gh_log" GH_STATE_DIR="$scratch/gh-state" \
    PATH="$scratch/bin:$PATH" bash "$init"
) > /dev/null

echo "native epic publication: ok"
