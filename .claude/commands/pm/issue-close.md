---
allowed-tools: Bash, Read, Write, LS
---

# Issue Close

Validate completion evidence, then close an eligible GitHub issue.

## Usage

```text
/pm:issue-close <issue_number> <completion_evidence>
```

GitHub is authoritative. Local task files are optional and never gate closure.
Completion evidence uses anchored `Field: value` entries separated by newlines
or semicolons.

Run validation and mutation as one block:

```bash
issue_number="${ARGUMENTS%% *}"
completion_evidence="${ARGUMENTS#"$issue_number"}"
completion_evidence="${completion_evidence# }"

case "$issue_number" in
  ''|*[!0-9]*) echo "❌ A numeric issue number is required" >&2; exit 1 ;;
esac

field_value() {
  local field="$1"
  local record="$2"

  printf '%s\n' "$record" | tr ';' '\n' | awk -v wanted="$field" '
    function trim(value) {
      sub(/^[[:space:]]+/, "", value)
      sub(/[[:space:]]+$/, "", value)
      return value
    }
    {
      line=$0
      sub(/^[[:space:]]*[-+][[:space:]]+/, "", line)
      separator=index(line, ":")
      if (!separator) next
      name=trim(substr(line, 1, separator - 1))
      if (tolower(name) == tolower(wanted)) value=trim(substr(line, separator + 1))
    }
    END { if (value != "") print value }
  '
}

normalized_value() {
  # Backticks are literal Markdown wrappers, not command substitutions.
  # shellcheck disable=SC2016
  printf '%s' "$1" |
    sed -E 's/^[[:space:]]+//; s/[[:space:].,;!?]+$//; s/^([*_~`]+)[[:space:]]*//; s/[[:space:]]*([*_~`]+)$//; s/^[[:space:]]+//; s/[[:space:].,;!?]+$//' |
    tr '[:upper:]' '[:lower:]' |
    tr -d '[:space:]'
}

is_chained_field() {
  local value
  value=$(printf '%s' "$1" | tr -d '*_`~' | tr '[:upper:]' '[:lower:]' |
    sed -E 's/^[[:space:]]*[-+][[:space:]]+//; s/^[[:space:]]+//; s/[[:space:]]+:/:/')
  case "$value" in
    'acceptance check:'*|'acceptance checks:'*|'result:'*|'parent acceptance audit:'*|'exact target:'*|'named controller:'*|'candidate identity:'*|'configuration identity:'*|'rollback identity:'*|'rollback procedure:'*|'stop rules:'*|'terminal result:'*|'cleanup evidence:'*) return 0 ;;
    *) return 1 ;;
  esac
}

has_meaningful_field() {
  local value normalized
  value=$(field_value "$1" "$2")
  [ -n "$value" ] || return 1
  is_chained_field "$value" && return 1
  normalized=$(normalized_value "$value")
  case "$normalized" in
    ''|'-'|'--'|'---'|'tbd'|'todo'|'pending'|'unknown'|'none'|'n/a'|'na'|'missing'|'absent'|'failed'|'failure'|'error'|'false'|'no'|'nil'|'null'|'unavailable'|'notavailable'|'notconfigured'|'notdone'|'notfound'|'notperformed'|'notrun'|'notset'|'incomplete') return 1 ;;
  esac
}

field_passed() {
  local value normalized
  value=$(field_value "$1" "$2")
  [ -n "$value" ] || return 1
  is_chained_field "$value" && return 1
  normalized=$(normalized_value "$value")
  case "$normalized" in
    pass|passed|success|successful|succeeded|complete|completed) return 0 ;;
    *) return 1 ;;
  esac
}

if ! has_meaningful_field "Acceptance checks" "$completion_evidence" &&
  ! has_meaningful_field "Acceptance check" "$completion_evidence"; then
  echo "❌ Completion evidence requires meaningful anchored Acceptance checks" >&2
  exit 1
fi
if ! field_passed "Result" "$completion_evidence"; then
  echo "❌ Completion evidence requires Result: passed" >&2
  exit 1
fi

issue_state=$(gh issue view "$issue_number" --json state --jq .state) || exit 1
[ "$issue_state" = OPEN ] || { echo "❌ Issue is not open" >&2; exit 1; }
open_blockers=$(gh api --paginate "repos/{owner}/{repo}/issues/$issue_number/dependencies/blocked_by" \
  --jq '.[] | select(.state != "closed") | .number') || exit 1
[ -z "$open_blockers" ] || { echo "❌ Native blockers remain open" >&2; exit 1; }
issue_labels=$(gh issue view "$issue_number" --json labels --jq '.labels[].name') || exit 1
category_count=$(printf '%s\n' "$issue_labels" | awk '$0 == "bug" || $0 == "enhancement" { n++ } END { print n + 0 }')
triage_count=$(printf '%s\n' "$issue_labels" | awk '/^(needs-triage|needs-info|ready-for-agent|ready-for-human|wontfix)$/ { n++ } END { print n + 0 }')
[ "$category_count" -eq 1 ] && [ "$triage_count" -eq 1 ] || { echo "❌ Issue labels violate the lifecycle contract" >&2; exit 1; }

if printf '%s\n' "$issue_labels" | grep -Fxq tracking; then
  open_children=$(gh api --paginate "repos/{owner}/{repo}/issues/$issue_number/sub_issues" \
    --jq '.[] | select(.state != "closed") | .number') || exit 1
  [ -z "$open_children" ] || { echo "❌ Tracking issue has open sub-issues" >&2; exit 1; }
  field_passed "Parent acceptance audit" "$completion_evidence" || {
    echo "❌ Tracking closure requires Parent acceptance audit: passed" >&2
    exit 1
  }
fi

if printf '%s\n' "$issue_labels" | grep -Fxq runtime; then
  runtime_record=$(gh issue view "$issue_number" --json comments \
    --jq '[.comments[].body] | join("\n")') || exit 1
  runtime_record="$runtime_record
$completion_evidence"
  for field in "Exact target" "Named controller" "Candidate identity" \
    "Configuration identity" "Rollback identity" "Rollback procedure" \
    "Stop rules" "Cleanup evidence"; do
    has_meaningful_field "$field" "$runtime_record" || {
      echo "❌ Runtime closure requires meaningful $field" >&2
      exit 1
    }
  done
  field_passed "Terminal result" "$runtime_record" || {
    echo "❌ Runtime closure requires Terminal result: passed" >&2
    exit 1
  }
fi

evidence_file=$(mktemp)
trap 'rm -f "$evidence_file"' EXIT
printf '## Completion evidence\n\n%s\n' "$completion_evidence" > "$evidence_file"
gh issue comment "$issue_number" --body-file "$evidence_file" || exit 1
gh issue close "$issue_number" --reason completed || exit 1
closed_state=$(gh issue view "$issue_number" --json state --jq .state) || exit 1
[ "$closed_state" = CLOSED ] || { echo "❌ GitHub readback is not CLOSED" >&2; exit 1; }
gh issue view "$issue_number" --json state,closedAt,comments,url
```

Only after the `CLOSED` readback may an existing local mirror be marked stale
or refreshed as optional enrichment. Do not close a parent as a child-side
effect.
