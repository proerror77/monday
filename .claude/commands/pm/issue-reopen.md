---
allowed-tools: Bash, Read, Write, LS
---

# Issue Reopen

Reopen a GitHub issue without requiring local frontmatter.

## Usage

```text
/pm:issue-reopen <issue_number> [reason]
```

Run the GitHub mutation and readback as one block:

```bash
issue_number="${ARGUMENTS%% *}"
reason="${ARGUMENTS#"$issue_number"}"
reason="${reason# }"
case "$issue_number" in
  ''|*[!0-9]*) echo "❌ A numeric issue number is required" >&2; exit 1 ;;
esac

issue_state=$(gh issue view "$issue_number" --json state --jq .state) || exit 1
case "$issue_state" in CLOSED) gh issue reopen "$issue_number" || exit 1 ;; OPEN) ;; *) echo "❌ Unknown GitHub issue state" >&2; exit 1 ;; esac
reopened_state=$(gh issue view "$issue_number" --json state --jq .state) || exit 1
[ "$reopened_state" = OPEN ] || { echo "❌ GitHub readback is not OPEN" >&2; exit 1; }

if [ -n "$reason" ]; then
  printf '## Reopen reason\n\n%s\n' "$reason" |
    gh issue comment "$issue_number" --body-file - || exit 1
fi

final_readback=$(gh issue view "$issue_number" --json state,updatedAt,url \
  --jq '[.state, .updatedAt, .url] | @tsv') || exit 1
case "$final_readback" in
  OPEN$'\t'*) printf '%s\n' "$final_readback" ;;
  *) echo "❌ Final GitHub readback is not OPEN" >&2; exit 1 ;;
esac
```

After GitHub reports `OPEN`, report an existing mirror as `Local mirror: stale
- refresh from GitHub before reuse`, or a missing mirror as `Local mirror:
absent (optional)`. Preserve local history; never treat it as authoritative.
