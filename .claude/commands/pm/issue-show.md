---
allowed-tools: Bash, Read, LS
---

# Issue Show

Display one issue and its live GitHub relationships.

## Usage

```text
/pm:issue-show <issue_number>
```

GitHub is authoritative. Run this without requiring `.claude/epics`:

```bash
issue_number="${ARGUMENTS%% *}"
case "$issue_number" in
  ''|*[!0-9]*) echo "❌ A numeric issue number is required" >&2; exit 1 ;;
esac

gh issue view "$issue_number" \
  --json number,title,state,stateReason,body,labels,assignees,parent,subIssues,blockedBy,blocking,closedByPullRequestsReferences,comments,createdAt,updatedAt,closedAt,url || exit 1

repo=$(gh repo view --json nameWithOwner -q .nameWithOwner) || exit 1
closing_prs=$(gh issue view "$issue_number" --json closedByPullRequestsReferences \
  --jq '.closedByPullRequestsReferences[].url') || exit 1
referencing_prs=$(gh api --paginate "repos/$repo/issues/$issue_number/timeline" \
  --jq '.[] | select(.event == "cross-referenced" and .source.issue.pull_request != null) | .source.issue.html_url') || exit 1
linked_prs=$(printf '%s\n%s\n' "$closing_prs" "$referencing_prs" | awk 'NF && !seen[$0]++')

while IFS= read -r pr_url; do
  [ -n "$pr_url" ] || continue
  gh pr view "$pr_url" \
    --json number,title,state,isDraft,mergedAt,headRefName,baseRefName,url || exit 1
done <<< "$linked_prs"
```

Present labels, assignees, parent, sub-issues, blocked-by, blocking, linked pull
requests, comments, and timestamps. Use `None` for empty native relationships.
An existing local mirror may be listed afterward as optional enrichment; its
absence and its status never override GitHub.
