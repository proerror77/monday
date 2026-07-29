---
allowed-tools: Bash, Read, LS
---

# Issue Status

Report concise live GitHub status for one or more issues.

## Usage

```text
/pm:issue-status <issue_number>[,<issue_number>...]
```

GitHub is authoritative. Run this without requiring `.claude/epics`:

```bash
IFS=',' read -r -a issue_numbers <<< "$ARGUMENTS"
for issue_number in "${issue_numbers[@]}"; do
  case "$issue_number" in
    ''|*[!0-9]*) echo "❌ A numeric issue number is required" >&2; exit 1 ;;
  esac

  gh issue view "$issue_number" \
    --json number,title,state,stateReason,body,labels,assignees,parent,subIssues,blockedBy,blocking,closedByPullRequestsReferences,comments,updatedAt,url || exit 1

  repo=$(gh repo view --json nameWithOwner -q .nameWithOwner) || exit 1
  closing_prs=$(gh issue view "$issue_number" --json closedByPullRequestsReferences \
    --jq '.closedByPullRequestsReferences[].url') || exit 1
  referencing_prs=$(gh api --paginate "repos/$repo/issues/$issue_number/timeline" \
    --jq '.[] | select(.event == "cross-referenced" and .source.issue.pull_request != null) | .source.issue.html_url') || exit 1
  linked_prs=$(printf '%s\n%s\n' "$closing_prs" "$referencing_prs" | awk 'NF && !seen[$0]++')

  while IFS= read -r pr_url; do
    [ -n "$pr_url" ] || continue
    gh pr view "$pr_url" --json number,title,state,isDraft,mergedAt,url || exit 1
  done <<< "$linked_prs"
done
```

Report labels, assignees, native parent/sub-issues, blockers, blocking issues,
and linked PR state. An open native blocker means `blocked`. Do not infer
completion from local percentages; mention a local mirror only as optional
enrichment.
