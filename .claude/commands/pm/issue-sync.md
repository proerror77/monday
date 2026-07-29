---
allowed-tools: Bash, Read, Write, LS
---

# Issue Sync

Post explicit new evidence to a GitHub issue.

## Usage

```text
/pm:issue-sync <issue_number> [evidence_file]
```

GitHub is authoritative. Missing local progress is a normal no-op; never infer
evidence from local status, percentages, commits, or timestamps.

```bash
issue_number="${ARGUMENTS%% *}"
evidence_file="${ARGUMENTS#"$issue_number"}"
evidence_file="${evidence_file# }"
case "$issue_number" in
  ''|*[!0-9]*) echo "❌ A numeric issue number is required" >&2; exit 1 ;;
esac

gh issue view "$issue_number" --json number,title,state,url || exit 1
if [ -z "$evidence_file" ]; then
  echo "No explicit new evidence; nothing to sync."
  echo "Local progress: ignored (optional)."
  exit 0
fi

if [ ! -f "$evidence_file" ] || [ ! -r "$evidence_file" ] || [ ! -s "$evidence_file" ]; then
  echo "❌ Evidence file must be a readable, non-empty regular file" >&2
  exit 1
fi
gh issue comment "$issue_number" --body-file "$evidence_file" || exit 1
gh issue view "$issue_number" --json comments,url || exit 1
```

Do not update task, epic, progress, or completion frontmatter as a side effect.
